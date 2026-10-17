# USB discovery regression: optional SCSI identity data

Reproduced 2026-10-17 on CachyOS kernel 7.2.9-2-cachyos with an unmounted
Kingston DataTraveler 3.0, 61,872,793,600 bytes, enumerated by Linux as `/dev/sda`.
The drive was inspected only; no physical disk was opened for writing.

## Cause and evidence

`lsblk` reported a USB disk, its serial, model and capacity. The nearest actual
USB device's sysfs `serial` attribute was readable. However, both
`/sys/class/block/sda/device/serial` and `device/wwid` existed and returned
`ENXIO` (No such device or address). The installed ForgeUSB build aborted the
entire discovery pass on the first optional attribute error, hiding all devices.
This error persisted through reconnecting, so additional retries could not fix it.

The Linux SCSI implementation returns `-ENXIO` when its optional VPD page 0x80
serial or page 0x83 identification data is absent. See `scsi_vpd_lun_serial`
and `scsi_vpd_lun_id` in the [pinned kernel source](https://code.googlesource.com/linux/torvalds/linux/+/f24ba334afafc70c3149e9db9c0cf8ecc6d52a09/drivers/scsi/scsi_lib.c#3376).
This source supports the optional-data interpretation; it does not imply that
the attached drive's internal firmware or boot behavior was analyzed.

## Fix and safety boundary

- Optional identity/description attributes treat `ENOENT` and `ENXIO` as absent.
  USB identity resolution then uses the already implemented nearest USB-device
  serial fallback. A hub/controller serial is never substituted.
- Required geometry, device numbers, graph edges and topology snapshots still
  fail closed. Permission errors, `EIO`, `ENODEV` and malformed/oversized metadata
  are not suppressed. No missing identity is invented; devices without a stable
  serial/WWID remain ineligible for physical writes.
- GUI discovery errors are tracked separately from operation errors. A successful
  refresh clears only the discovery error. A failed physical-device discovery
  revokes its selection, plan and typed confirmation. Recovery never reselects
  a target or restores old approval.

Four added regression tests cover optional VPD absence, unexpected read failures,
GUI recovery without hiding an operation failure, and invalidation/recovery of a
physical selection. The existing nearest-device/hub isolation test now exercises
the ENXIO fallback, alongside the retained admission safety tests.

Validation outcomes and installed binary hashes are in
[usb-detection-validation.json](usb-detection-validation.json). Live hardware
checks are read-only enumeration and planning, not physical write/boot tests.

![Installed GUI sees one USB, with no automatic selection](usb-detection.png)
