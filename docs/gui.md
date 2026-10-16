# Desktop application

ForgeUSB uses Iced 0.13 with its CPU renderer (`tiny-skia`) and a Tokio-backed
runtime. See the [Iced API documentation](https://docs.rs/iced/0.13.1/iced/).
The executable must run as the ordinary desktop user. It refuses UID 0.

```console
cargo run -p forgeusb-gui
cargo run -p forgeusb-gui -- --image /absolute/path/to/image.iso
cargo run -p forgeusb-gui -- --image /absolute/path/to/disk.img --test-output /absolute/path/to/new-test.img
```

An image path is entered directly; this version has no graphical file chooser.
Tab/Shift+Tab move between text fields. Ctrl+O focuses the image path, Enter
analyzes it, Alt+Up/Down explicitly cycles eligible USB targets, and Alt+F
switches disposable-file mode. Ctrl+P previews, F8 starts only after the exact
confirmation is entered, and Escape cancels a running write. Enter in the
confirmation field also starts the confirmed operation. Ctrl+R refreshes
devices. Alt+T changes theme, Alt+L expands the log, and Alt+A expands advanced
details. Full screen-reader integration has not been validated.

1. Enter the source path and choose **Analyze image**. Inspection and SHA-256
   calculation run on a worker thread.
2. Select a removable USB explicitly, or enable disposable file mode and enter
   a path that does not exist. No physical target is ever selected by default.
3. Choose **Preview exact operation**. This only builds a read-only plan.
   The form scrolls to the preview and focuses its confirmation field.
4. Review the source, target, exact byte count, strategy and warnings. Type the
   complete confirmation displayed by the engine.
5. Start the confirmed operation. Physical writes invoke the installed fixed
   `/usr/lib/forgeusb/forgeusb-helper` with `/usr/bin/pkexec`. Disposable-file
   writes use the same engine without privilege elevation.

The destination list refreshes from live sysfs/procfs every three seconds on a
background worker. Ineligible devices are displayed with their refusal reasons.
A changed/disappeared selected device clears the preview and confirmation.
The engine independently revalidates source and target immediately before I/O.

Progress is delivered through a bounded channel and polled every 100 ms by the
event loop. Status and progress remain in a fixed footer while the form scrolls.
**Cancel safely** requests cooperative cancellation at an I/O
boundary. Closing the window during writing requests cancellation and waits for
the engine/helper to stop before exiting. An interrupted target may be incomplete;
there is no rollback claim. A failed or missing completion record cannot produce
a successful verification message.

The expandable log retains the latest 160 state/error messages in memory.
It is not a persistent forensic log. No telemetry is sent.

## Current strategy limits

Partition scheme, firmware and filesystem show **From image** or **Preserve image
layout** because raw mode copies the source disk layout exactly. These fields do
not pretend to change a raw image's bootloader, filesystem or volume label.
The advanced panel explains the read-back boundary. Verification compares the
written image range; unused trailing capacity is not verified, and a byte match
does not prove boot compatibility.

The first GUI supports raw-image execution. Filesystem extraction, Windows WIM
splitting, persistence and BIOS/UEFI bootloader installation require separately
implemented and tested strategies. Mounted USB partitions must be unmounted
through the desktop before planning; the GUI does not silently unmount them.

## Safe startup smoke test

```console
cargo run -p forgeusb-gui -- --smoke-test
```

This opens the real GUI, runs its event loop and read-only device discovery for
ten seconds, then exits. The Start action is disabled in smoke mode even if a
plan has been prepared. An optional `--image` performs real read-only analysis.
It never writes media automatically. A graphical session (or a disposable Xvfb
display) is required.

The local Xvfb smoke test on 2026-10-17 ran the real unprivileged executable,
analyzed the owned 1 MiB BIOS fixture, discovered one storage device, and exercised
Ctrl+P to build the real read-only plan. The supplied disposable output path
remained absent. Screenshots were visually inspected: [main form](gui-smoke.png)
and [exact plan](gui-plan-smoke.png). Logs are in `test-output/gui-smoke.log` and
`test-output/gui-keyboard-smoke.log`. This covers desktop startup, image analysis,
read-only planning and disabled confirmation; physical writes and the Polkit
authorization dialog were not exercised by this GUI smoke test.
