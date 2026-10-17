# Compact Rufus-style GUI QA

Date: 2026-10-17. Target: native Rust/Iced ForgeUSB, not a web prototype.

## Visual truth and comparison state

- Official reference: https://rufus.ie/pics/screenshot1_en.png, linked from
  https://rufus.ie/en/. The official screenshot shows Rufus 3.20's standard light
  dialog. Its control order also matches the pinned Rufus 4.15 resource dialog at
  `references/rufus/src/rufus.rc:33` (commit `6d8fbf98305ff37eb531c45cbd6ff44563c53917`).
- Source capture retained locally: `test-output/ui-rework/rufus-original.png`.
  Original 950 × 1234 at 2×; normalized to 475 × 617 at 1×; the Windows title bar
  is cropped by 24 px, giving a 475 × 593 client region. No arbitrary vertical
  scaling was used.
- Implementation: `docs/gui-smoke.png`, 475 × 560 client pixels at 1×, and
  `docs/gui-plan-smoke.png` in the same viewport. Captured from the real release
  binary under Xvfb. Linux window-manager chrome is outside this comparison.
- Combined comparison inputs: `test-output/ui-rework/comparison-pass1.png` and
  `comparison-final.png`; focused typography/control comparison in
  `comparison-controls.png`. The source and implementation were actually opened
  together in the combined images, not judged from code alone.
- State differences: the official screenshot is a Windows ISO copy in progress
  with advanced format options expanded. The implementation captures real DD
  image inspection at idle, with advanced sections collapsed. The test-file
  capture supports a read-only confirmation; the normal main capture has no USB
  selected. There is no invented USB identity or Windows installation support.
  These intentional capability/state differences preclude a pixel-identical
  Windows screenshot claim.

## Comparison history

1. First native capture: the large cards, header and accent colors had already
   been removed, but control height/spacing was too loose and section headings
   were too light [P2]. Main window was 475 × 600. Evidence: comparison-pass1.png.
2. Fix: 20 px fields, 6 px group gaps, bold 19 px section labels and a 475 × 560
   default window. The final side-by-side capture confirms the reference's
   compact label/control rhythm, two-column partition/filesystem rows, gray
   surface, black section dividers and lower status/action region.
3. The real confirmation capture was checked for source/target byte counts,
   wrapped confirmation text, reachable START/CANCEL and retained warning
   scrolling. Native SELECT was exercised end to end on a private display;
   it delivered the selected source to analysis and the shared plan engine.

## Required fidelity surfaces

- **Typography:** Noto Sans 12 px labels and control values; bold 19 px section
  titles. Segoe UI and the Windows title bar are not bundled. Text is readable
  without clipping; source paths retain normal text-input horizontal scrolling.
- **Spacing/layout:** single 475 px column, 14 px margins, compact controls and
  matched two-column tracks. Status and START/CLOSE stay visible at the 440 × 540
  minimum window size; advanced properties scroll independently. The collapsed
  DD form is shorter than the reference's expanded Windows form by design.
- **Colors/tokens:** #f0f0f0 dialog surface, white editable fields, gray read-only
  fields and borders, black text/dividers, standard blue focus and green progress.
  No cards, gradients, rounded panels, promotional header or decorative accents.
- **Assets:** no copied Rufus artwork is used. The original ForgeUSB USB icon is
  embedded for X11 and installed as SVG/PNG for the application menu and Wayland.
  Native Linux window chrome is provided by the desktop. The small toolbar uses
  actual labeled Log/Refresh buttons instead of approximated Windows icon art.
- **Content:** Device, Boot selection, Image option, Partition scheme, Target
  system, Volume label, File system, Cluster size, Status and START/CLOSE follow
  the original order. Raw-image properties accurately show From image and are
  disabled; unsupported ISO extraction cannot silently activate another method.

## Interaction validation

- Real native KDE file chooser → source inspection → read-only confirmation.
- Source/result paths asserted by the smoke harness, not screenshots alone.
- A separate real GUI write to a new owned 4 MiB temporary file reached DONE;
  engine read-back, independent CLI verification and SHA-256 comparison passed.
  The fixture directory was removed; no physical storage was accessed for writing.
- Exact confirmation remains required; review dismissal revokes the plan.
- No auto-selected device; no physical target is planned by device-mode smoke.
- No output file is created by any GUI smoke; no operation is started in smoke
  mode. Picker cancellation/window-close state is covered by the GUI unit tests.
- Formatting, strict workspace Clippy, 44 workspace tests and locked release
  build pass. The physical Polkit flow and other desktop toolkits remain outside
  this UI acceptance evidence.

## Findings and remaining polish

No actionable P0/P1/P2 findings remain within the selected native DD-mode scope.
Expected platform/capability differences are explicitly listed above. Windows
control rendering, unavailable format/boot strategies and cross-desktop native
picker appearance are not represented as supported or pixel-identical behavior.

Implementation checklist: compact light default; real SELECT; fixed Status and
START/CLOSE; truthful read-only fields; modal exact confirmation; retained safety
engine; inspected combined reference/implementation captures.

final result: passed
