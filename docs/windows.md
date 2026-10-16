# Windows installation media: current staging boundary

Windows 10/11 ISO-to-device creation is not implemented. A plain Windows installation ISO is an optical filesystem image; writing it byte for byte does not implement Rufus's filesystem-based installer strategy. The strategy policy therefore refuses an executable Windows raw fallback.

`forgeusb-windows` currently provides two usable library stages:

```rust,no_run
use std::{path::Path, time::Duration};
use forgeusb_core::Cancellation;

let report = forgeusb_windows::inspect_extracted_tree(Path::new("/home/user/windows-tree"))?;
let parts = forgeusb_windows::split_wim(
    Path::new("/home/user/windows-tree/sources/install.wim"),
    Path::new("/home/user/windows-split-new"),
    &Cancellation::default(),
    Duration::from_secs(3600),
)?;
# Ok::<(), anyhow::Error>(())
```

Run staging as an ordinary user, on a quiescent caller-owned tree. The split output directory must not already exist. WIM source must be owned by the caller and not group/world writable. The wrapper requires the OS-provided root-owned `/usr/bin/wimlib-imagex`, clears inherited environment variables and invokes it directly without a shell. No device is opened, mounted, partitioned or formatted.

Tree inspection limits recursion/entries, rejects symlinks/special files and case-insensitive name collisions, checks FAT/Windows names, totals sizes with checked arithmetic, inspects WIM magic and verifies that fallback EFI filenames match their PE machine. It accepts install.wim, install.esd or the first install.swm header. It does **not** validate complete WIM resources, every split part, Microsoft signatures, boot.wim authenticity, Secure Boot trust or actual firmware execution.

The split stage requests 3800 MiB parts with `--check`. wimlib verifies a source integrity table when present and emits integrity information for output parts. A single resource can exceed a requested size, so ForgeUSB rejects any resulting part above FAT32's 4 GiB-1 file limit. Cancellation/timeout kills and reaps the process; partial output remains in the named staging directory. An oversized ESD or other non-WIM file requires a different supported strategy; arbitrary file splitting would break the installer. [wimlib manual](https://wimlib.net/man1/wimsplit.html)

No Windows ISO was supplied for installer startup testing, and wimlib is an optional dependency. Synthetic header/tree tests verify policy and bounds only. Before completing this milestone, integrate safe ISO/UDF extraction, partition/format executors, file-by-file copy/digest validation, split-part integrity checks and OVMF boot with an authorized Windows ISO. NTFS/UEFI:NTFS also requires pinned bootloader provenance, architecture, licenses, signature and revocation checks.
