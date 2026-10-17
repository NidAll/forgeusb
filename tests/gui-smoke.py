#!/usr/bin/env python3
"""Exercise the real, unprivileged GUI on a private Xvfb display.

Only --smoke-test is launched: Start remains disabled inside the application.
The target is a fresh pathname in an exclusively owned TemporaryDirectory.
Optional keyboard input selects the source in the native file chooser and/or
sends Ctrl+P. It never types a destructive confirmation or presses Start.

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
    parser.add_argument("--browse", action="store_true", help="select the source using kdialog or zenity")
    parser.add_argument("--device-mode", action="store_true", help="capture the normal USB selector without choosing a device")
    parser.add_argument("--small", action="store_true", help="exercise the minimum 440x540 window size")
    parser.add_argument("--advanced", action="store_true", help="show advanced drive properties")
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


def keyboard_input(window_title: str, *, keys: tuple[str, ...] = (), value: str = "", dimensions: tuple[int, int] | None = None) -> None:
    """Send bounded test input only to a named window on the private display."""
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
    x.XInternAtom.argtypes = [c.c_void_p, c.c_char_p, c.c_int]
    x.XInternAtom.restype = c.c_ulong
    x.XGetWindowProperty.argtypes = [c.c_void_p, c.c_ulong, c.c_ulong, c.c_long,
        c.c_long, c.c_int, c.c_ulong, c.POINTER(c.c_ulong), c.POINTER(c.c_int),
        c.POINTER(c.c_ulong), c.POINTER(c.c_ulong), c.POINTER(c.c_void_p)]
    x.XFree.argtypes = [c.c_void_p]
    x.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
    x.XResizeWindow.argtypes = [c.c_void_p, c.c_ulong, c.c_uint, c.c_uint]
    x.XStringToKeysym.argtypes = [c.c_char_p]
    x.XStringToKeysym.restype = c.c_ulong
    x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
    x.XKeysymToKeycode.restype = c.c_ubyte
    x.XKeycodeToKeysym.argtypes = [c.c_void_p, c.c_ubyte, c.c_int]
    x.XKeycodeToKeysym.restype = c.c_ulong
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
        titles = []
        try:
            for index in range(count.value):
                title = ""
                name = c.c_void_p()
                if x.XFetchName(display, children[index], c.byref(name)) and name.value:
                    title = c.string_at(name).decode(errors="replace")
                    x.XFree(name)
                # Qt uses the UTF-8 title property for non-Latin-1 dialog titles.
                atom = x.XInternAtom(display, b"_NET_WM_NAME", 1)
                actual_type, count_bytes, remaining = c.c_ulong(), c.c_ulong(), c.c_ulong()
                actual_format, value_bytes = c.c_int(), c.c_void_p()
                if atom and x.XGetWindowProperty(display, children[index], atom, 0, 1024, 0, 0,
                        c.byref(actual_type), c.byref(actual_format), c.byref(count_bytes),
                        c.byref(remaining), c.byref(value_bytes)) == 0 and value_bytes.value:
                    try:
                        if actual_format.value == 8:
                            title = c.string_at(value_bytes, count_bytes.value).decode(errors="replace")
                    finally:
                        x.XFree(value_bytes)
                titles.append(title)
                if title.startswith(window_title):
                    window = children[index]
                    break
        finally:
            x.XFree(children)
        require(window is not None, f"{window_title} window not found on private display; available: {titles}")
        x.XSetInputFocus(display, window, 2, 0)
        if dimensions is not None:
            x.XResizeWindow(display, window, *dimensions)
        codes = [x.XKeysymToKeycode(display, x.XStringToKeysym(key.encode())) for key in keys]
        require(all(codes), "private X server has no expected keymap")
        for key, pressed in [(key, 1) for key in codes] + [(key, 0) for key in reversed(codes)]:
            require(bool(xt.XTestFakeKeyEvent(display, key, pressed, 0)),
                    "could not send the test shortcut")
        shift = x.XKeysymToKeycode(display, x.XStringToKeysym(b"Shift_L"))
        for character in value:
            require(32 <= ord(character) < 127, "native chooser test requires an ASCII fixture path")
            code = x.XKeysymToKeycode(display, ord(character))
            require(bool(code), "fixture character has no keycode")
            shifted = x.XKeycodeToKeysym(display, code, 0) != ord(character)
            require(not shifted or x.XKeycodeToKeysym(display, code, 1) == ord(character), "unexpected fixture keymap")
            if shifted:
                xt.XTestFakeKeyEvent(display, shift, 1, 0)
            xt.XTestFakeKeyEvent(display, code, 1, 0)
            xt.XTestFakeKeyEvent(display, code, 0, 0)
            if shifted:
                xt.XTestFakeKeyEvent(display, shift, 0, 0)
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
    env["QT_QPA_PLATFORM"] = "xcb"
    screenshots = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="forgeusb-owned-gui-smoke-") as directory:
        target = Path(directory) / "never-created-output.img"
        require(not target.exists(), "test target unexpectedly exists")
        log_path = args.output_dir / "gui.log"
        with log_path.open("w", encoding="utf-8") as log:
            command = [str(args.binary), "--image", "" if args.browse else str(args.source), "--smoke-test"]
            if not args.device_mode:
                command.extend(["--test-output", str(target)])
            app = subprocess.Popen(
                command,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                if args.preview or args.screenshot or args.browse or args.small or args.advanced:
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
                    if args.small:
                        keyboard_input("ForgeUSB", dimensions=(440, 540))
                    if args.advanced:
                        keyboard_input("ForgeUSB", keys=("Alt_L", "a"))
                    if args.small or args.advanced:
                        time.sleep(0.2)
                        capture(frame)
                    if args.browse:
                        keyboard_input("ForgeUSB", keys=("Control_L", "o"))
                        deadline = time.monotonic() + 4
                        while True:
                            try:
                                name_keys = ("Alt_L", "n") if Path("/usr/bin/kdialog").is_file() else ("Control_L", "l")
                                keyboard_input("Select an image", keys=name_keys)
                                break
                            except RuntimeError as error:
                                require(time.monotonic() < deadline, f"native file chooser did not open: {error}")
                                time.sleep(0.2)
                        keyboard_input("Select an image", keys=("Control_L", "a"))
                        keyboard_input("Select an image", value=str(args.source))
                        keyboard_input("Select an image", keys=("Return",))
                        time.sleep(0.7)
                        if args.screenshot:
                            capture(frame)
                    if args.screenshot:
                        screenshots.append(str(frame))
                    if args.preview:
                        keyboard_input("ForgeUSB", keys=("Control_L", "p"))
                        # A true shared-engine plan runs asynchronously and hashes the source.
                        time.sleep(2.5)
                        if args.screenshot:
                            preview = args.output_dir / "gui-preview.png"
                            capture(preview)
                            require(preview.stat().st_size > 4096, "preview screenshot is empty")
                            screenshots.append(str(preview))
                returncode = app.wait(timeout=25)
            except Exception:
                if args.screenshot and app.poll() is None:
                    capture(args.output_dir / "gui-failure.png")
                raise
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
        prefix = "ForgeUSB GUI smoke state: "
        states = [json.loads(line[len(prefix):]) for line in log_path.read_text(encoding="utf-8").splitlines()
                  if line.startswith(prefix)]
        require(bool(states) and states[-1]["image"] == str(args.source), "source image was not analyzed")
        require(not states[-1]["running"], "smoke mode unexpectedly started an operation")
        if args.preview:
            require(states[-1]["plan_ready"] and states[-1]["confirmation_open"],
                    "read-only confirmation was not prepared")
    return {
        "ok": True,
        "binary": str(args.binary),
        "source": str(args.source),
        "exit_code": returncode,
        "target_created": False,
        "private_display": display,
        "preview_shortcut_sent": args.preview,
        "native_file_chooser_exercised": args.browse,
        "normal_device_mode": args.device_mode,
        "small_window": args.small,
        "advanced_properties": args.advanced,
        "screenshots": screenshots,
        "preview_visual_review_required": bool(args.preview and args.screenshot),
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "log": str(log_path),
    }


def main() -> int:
    args = arguments()
    require(os.geteuid() != 0, "GUI tests must run as the ordinary desktop user")
    require(not (args.device_mode and args.preview), "device-mode smoke never plans operations against physical devices")
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
    if args.browse:
        command.append("--browse")
    if args.device_mode:
        command.append("--device-mode")
    if args.small:
        command.append("--small")
    if args.advanced:
        command.append("--advanced")
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
