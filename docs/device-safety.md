# Device safety and current admission policy

ForgeUSB displays whole physical disks obtained from `/sys/class/block`. USB
transport is established from kernel sysfs ancestors, not a filename or the
`removable` flag. Internal disks are visible with explanatory refusal reasons;
partitions, loop, zram, device mapper, and other virtual devices are never choices.
Neither device ordering nor discovery automatically selects a target.

Discovery in the normal desktop process is preliminary. At a nonzero effective
UID, `eligible=true` means that visible mounts, swaps, topology, and device facts
allow a plan. Normal Linux permissions often prevent reading PID 1's mount
namespace link or mountinfo. That missing privileged evidence does not cause a
blanket refusal during desktop enumeration. A known namespace mismatch still
refuses, as do unknown visible filesystem backing, incomplete topology, and every
observed mounted/system-storage hazard.

A writable block-device open requires effective UID 0 before any device access.
The narrowly scoped helper repeats discovery as root and requires authoritative
PID 1 namespace equality and complete host mount visibility. Missing privileged
evidence fails closed there. Disk-group membership cannot bypass this boundary.
A preliminary plan is never sufficient authorization for a physical write.

An authoritative physical operation requires all of the following:

* A direct `/dev/<kernel-name>` block node, without a symlink.
* USB transport, nonzero capacity, 512-byte logical sectors, writable kernel state, and a stable serial or
  WWID. USB fallback serials come from the nearest actual USB device, never an
  upstream hub or host controller. Duplicate USB identities are refused. Some otherwise usable USB sticks
  have no unique identity and are deliberately unsupported.
* No mounted filesystem, active swap, or active kernel holder in the entire
  connected storage component. Partition ancestry, device mapper/LUKS/LVM
  slaves and holders, mdraid, and all registered Btrfs filesystem members are
  joined before evaluating use. A swap file protects its backing mount and the
  full connected storage component. This includes `/`, `/home`, `/boot`, and any
  other observed mount.
* Complete mount and swap information from `/proc/self/mountinfo`,
  `/proc/1/mountinfo`, and `/proc/swaps`. Mount paths use procfs octal decoding.
  Btrfs's anonymous filesystem device numbers resolve through the mount's block
  source and `/sys/fs/btrfs/*/devices`. Unknown active block sources, inaccessible
  mount information, unsupported overlay backing, and unstable sysfs graphs fail
  closed. Run outside a storage-hiding container; such environments cannot prove
  complete host storage safety.

Discovery compares two complete kernel topology snapshots around reading usage.
The reviewed identity contains device major/minor numbers, canonical sysfs path,
serial/WWID, capacity, logical sector size, and every node and edge in the associated topology. A
physical open rediscovers eligibility, opens with `O_NOFOLLOW | O_CLOEXEC |
O_EXCL`, checks the descriptor's block type and `st_rdev`, takes nonblocking
exclusive `flock`, checks `BLKGETSIZE64` and `BLKSSZGET`, and repeats discovery. Any changed
identity, model/vendor, capacity, or topology requires a new plan and confirmation.

Only 512-byte target logical sectors are currently supported. The image parser
uses 512-byte disk LBAs. A 4Kn USB SSD must be refused rather than copying a
512-byte GPT layout and claiming compatible media. Physical-sector alignment
(512e devices with 4096-byte physical sectors) remains supported because their
logical sectors are 512 bytes.

The descriptor retains Linux's exclusive block claim until closed. This prevents
new mounts and conflicting kernel holders while the operation runs. The flock
also excludes cooperating raw writers. A separate privileged process can still
perform raw I/O without respecting an advisory lock; ForgeUSB cannot claim
protection against a malicious root process or a compromised kernel. Linux
mount namespaces outside the caller and PID 1, inaccessible filesystem stacks,
and hidden device topologies cannot be exhaustively reconstructed from these
interfaces. Do not run the privileged helper inside containers or private mount
namespaces, and review device identity immediately before authorizing a write.

No function automatically unmounts partitions. Use the desktop's trusted UDisks
unmount action, refresh discovery, then review a new plan. The GUI remains
unprivileged. Raw operations are delegated to a scoped helper; the application
must require the model, direct path, and capacity confirmation before invoking
it. The `eligible` field is provisional outside the privileged helper and is
never user authorization. Root discovery and `revalidate` enforce authoritative
checks; `open_exclusive(..., true)` independently requires the root boundary.

Filesystem information in discovery comes from current mounts or the udev
ID_FS_TYPE cache; an unmounted unknown filesystem remains unknown. It is not a
fresh filesystem integrity probe. `DeviceMonitor::sample` supplies background
polling snapshots for insertion, removal, mount, and identity changes.

Unit tests use synthetic topology and procfs records and disposable regular
files. No unit test opens a physical block disk for writing, attaches a loop
device, unmounts storage, or changes a partition table. Root storage on the
development machine is not a test target. Physical unplug/replug, UDisks,
Polkit integration, and raw block-device exclusivity still require a separately
authorized disposable-device acceptance test.

## Read-only development evidence (2026-10-16)

`cargo run -p forgeusb-device --example discover` succeeded in the development
sandbox and displayed the one observed physical disk, `/dev/nvme0n1`, at
512110190592 bytes. It was refused as non-USB and as backing active filesystems
and swap. The `/boot` partition was identified as vfat; the Btrfs partition was
identified as backing `/`, `/home`, other mounted subvolumes, and the active swap
file. The sandbox hides physical `/dev` block nodes, producing an additional
explicit node-unavailable refusal. Self/PID 1 mount namespace comparison worked
in this environment; no unsupported namespace assumption was silently accepted.
No USB device was attached, so this is enumeration/refusal evidence, not a USB
write, hotplug hardware, or boot acceptance result.


## Desktop namespace acceptance correction (2026-10-17)

Host testing outside the sandbox established that ordinary UID 1000 cannot read
`/proc/1/ns/mnt`; the earlier sandbox had a user-owned PID 1 and did not reproduce
this permission boundary. Treating that absence as definitive refusal made all
normal desktop physical plans unusable. Desktop eligibility is now explicitly
preliminary, while the root-only writable-open boundary retains the complete
PID 1 checks. Injectable tests cover unprivileged permission denial, authoritative
permission denial, known namespace mismatch, malformed visible mountinfo, and
refusal of unprivileged writable opens before any target access. This correction
does not authorize a USB write or replace privileged-helper acceptance testing.

A read-only host run of `cargo run -p forgeusb-device --example discover` after
this correction succeeded as the desktop user. It reported the host NVMe only,
with non-USB, mounted-storage, and active-swap refusals. It did not add a global
PID 1 namespace refusal. The host `/dev` node was visible, unlike the sandbox.
No USB device was present, so eligible-USB admission remains a synthetic-policy
result pending authorized removable-device acceptance.
