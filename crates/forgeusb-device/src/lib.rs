//! Linux block discovery and fail-closed admission for physical writes.
//!
//! Discovery never opens a device for writing. Only [`open_exclusive`] opens a
//! block device, after reconstructing the kernel storage graph and revalidating
//! the user's selected identity. No API in this crate automatically unmounts.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct TopologyNode {
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub sysfs_path: PathBuf,
    pub capacity: u64,
    pub logical_sector_size: u32,
    pub connections: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub major: u32,
    pub minor: u32,
    pub sysfs_path: PathBuf,
    pub capacity: u64,
    pub logical_sector_size: u32,
    pub serial: Option<String>,
    pub wwid: Option<String>,
    pub topology: Vec<TopologyNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Partition {
    pub path: PathBuf,
    pub capacity: u64,
    /// Observed mount filesystem or udev's cached ID_FS_TYPE; not a fresh probe.
    pub filesystem: Option<String>,
    pub mounts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub path: PathBuf,
    pub model: String,
    pub vendor: String,
    pub capacity: u64,
    pub identity: Identity,
    pub eligible: bool,
    pub reasons: Vec<String>,
    pub mounts: Vec<String>,
    pub partitions: Vec<Partition>,
    pub removable: bool,
    pub transport: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    name: String,
    dev: (u32, u32),
    canonical: PathBuf,
    capacity: u64,
    logical_sector_size: u32,
    partition: bool,
    parent: Option<String>,
    connections: BTreeSet<String>,
    holders: BTreeSet<String>,
    usb: bool,
    removable: bool,
    read_only: bool,
    serial: Option<String>,
    wwid: Option<String>,
    model: String,
    vendor: String,
}

type Graph = BTreeMap<String, Node>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    dev: (u32, u32),
    point: String,
    fs: String,
    source: String,
}

#[derive(Default)]
struct Usage {
    mounts: BTreeMap<String, BTreeSet<String>>,
    filesystems: BTreeMap<String, String>,
    swap: BTreeSet<String>,
    uncertainties: BTreeSet<String>,
}

/// Enumerate whole physical disks, including ineligible internal disks for an
/// explanatory display. Partitions and virtual disks are not device choices.
///
/// A changing/incomplete sysfs topology fails discovery rather than returning a
/// partially eligible list. Unknown visible active storage sources disable writes.
/// At nonzero effective UID, eligibility is provisional: inaccessible PID 1
/// namespace information does not hide a device from desktop planning. Writable
/// block opens always require UID 0 and repeat authoritative system-wide checks.
pub fn discover() -> Result<Vec<Device>> {
    let mut last_error = None;
    for _ in 0..3 {
        match discover_once() {
            Ok(devices) => return Ok(devices),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("discovery loop always runs"))
        .context("cannot establish a stable storage topology; reconnect and retry")
}

fn discover_once() -> Result<Vec<Device>> {
    let graph = read_graph()?;
    let usage = read_usage(&graph)?;
    ensure!(
        graph == read_graph()?,
        "storage topology changed during discovery"
    );
    let mut devices = Vec::new();
    for node in graph
        .values()
        .filter(|node| !node.partition && !node.canonical.starts_with("/sys/devices/virtual"))
    {
        let names = component(&graph, &node.name);
        let (mut reasons, mounts) = admission(&graph, node, &usage);
        let path = Path::new("/dev").join(&node.name);
        if let Err(error) = validate_device_node(&path, node.dev) {
            reasons.insert(format!("device node unavailable or unsafe: {error:#}"));
        }
        let topology = names
            .iter()
            .map(|name| {
                let n = &graph[name];
                TopologyNode {
                    name: n.name.clone(),
                    major: n.dev.0,
                    minor: n.dev.1,
                    sysfs_path: n.canonical.clone(),
                    capacity: n.capacity,
                    logical_sector_size: n.logical_sector_size,
                    connections: n.connections.iter().cloned().collect(),
                }
            })
            .collect();
        let partitions = graph
            .values()
            .filter(|n| n.parent.as_deref() == Some(&node.name))
            .map(|n| Partition {
                path: Path::new("/dev").join(&n.name),
                capacity: n.capacity,
                filesystem: usage
                    .filesystems
                    .get(&n.name)
                    .cloned()
                    .or_else(|| udev_filesystem(n.dev)),
                mounts: usage
                    .mounts
                    .get(&n.name)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default(),
            })
            .collect();
        devices.push(Device {
            path,
            model: node.model.clone(),
            vendor: node.vendor.clone(),
            capacity: node.capacity,
            identity: Identity {
                major: node.dev.0,
                minor: node.dev.1,
                sysfs_path: node.canonical.clone(),
                capacity: node.capacity,
                logical_sector_size: node.logical_sector_size,
                serial: node.serial.clone(),
                wwid: node.wwid.clone(),
                topology,
            },
            eligible: reasons.is_empty(),
            reasons: reasons.into_iter().collect(),
            mounts: mounts.into_iter().collect(),
            partitions,
            removable: node.removable,
            transport: if node.usb { "usb" } else { "non-usb" }.into(),
        });
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(devices)
}

fn admission(graph: &Graph, node: &Node, usage: &Usage) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut reasons = usage.uncertainties.clone();
    if !node.usb {
        reasons.insert("not connected through USB; internal disks are never eligible".into());
    }
    if node.logical_sector_size != 512 {
        reasons.insert(format!("{}-byte logical sectors are unsupported; raw image parsing currently requires 512-byte target sectors", node.logical_sector_size));
    }
    if node.capacity == 0 {
        reasons.insert("device has zero capacity or no media".into());
    }
    if node.read_only {
        reasons.insert("kernel reports a read-only device".into());
    }
    if node.serial.is_none() && node.wwid.is_none() {
        reasons.insert("no stable serial or WWID; physical writes are disabled".into());
    }
    if graph.values().any(|other| {
        other.name != node.name
            && !other.partition
            && other.usb
            && other.serial == node.serial
            && other.wwid == node.wwid
    }) {
        reasons.insert("stable identity is duplicated by another USB device".into());
    }
    let mut mounts = BTreeSet::new();
    for name in component(graph, &node.name) {
        if let Some(points) = usage.mounts.get(&name) {
            mounts.extend(points.iter().cloned());
            reasons.insert(format!("storage component {name} has mounted filesystems"));
        }
        if usage.swap.contains(&name) {
            reasons.insert(format!("storage component {name} backs active swap"));
        }
        if let Some(related) = graph.get(&name)
            && !related.holders.is_empty()
        {
            reasons.insert(format!(
                "storage component {name} has active kernel holders"
            ));
        }
    }
    (reasons, mounts)
}

fn text(path: &Path) -> Result<String> {
    let value = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        value.len() <= 1024 * 1024,
        "unexpectedly large kernel attribute"
    );
    Ok(value.trim().to_owned())
}

fn optional_text(path: &Path) -> Result<Option<String>> {
    optional_text_value(path, fs::read_to_string(path))
}

fn optional_text_value(path: &Path, value: std::io::Result<String>) -> Result<Option<String>> {
    match value {
        Ok(value) => {
            ensure!(value.len() <= 64 * 1024, "oversized identity attribute");
            let value = value.trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        }
        // SCSI serial/WWID sysfs attributes can exist but return ENXIO when
        // their optional VPD data is unavailable. This is not a topology error.
        // Required geometry/graph attributes still use text() and fail closed.
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ENXIO) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn dev_number(value: &str) -> Result<(u32, u32)> {
    let (major, minor) = value.split_once(':').context("invalid device number")?;
    Ok((major.parse()?, minor.parse()?))
}

fn list_names(path: &Path) -> Result<BTreeSet<String>> {
    fs::read_dir(path)?
        .map(|entry| {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF8 sysfs block name"))?;
            ensure!(
                !name.is_empty() && !name.contains('/'),
                "invalid sysfs name"
            );
            Ok(name)
        })
        .collect()
}

fn read_graph() -> Result<Graph> {
    let root = Path::new("/sys/class/block");
    let mut graph = Graph::new();
    for name in list_names(root)? {
        let path = root.join(&name);
        let canonical = fs::canonicalize(&path)?;
        ensure!(
            canonical.starts_with("/sys/devices"),
            "unexpected block sysfs path"
        );
        let dev = dev_number(&text(&path.join("dev"))?)?;
        let sectors: u64 = text(&path.join("size"))?.parse()?;
        let capacity = sectors
            .checked_mul(512)
            .context("kernel capacity overflow")?;
        let partition = path.join("partition").try_exists()?;
        let parent = if partition {
            Some(
                canonical
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str())
                    .context("partition parent missing")?
                    .to_owned(),
            )
        } else {
            None
        };
        let queue = if let Some(parent) = &parent {
            root.join(parent).join("queue")
        } else {
            path.join("queue")
        };
        let logical_sector_size = text(&queue.join("logical_block_size"))?.parse()?;
        ensure!(logical_sector_size > 0, "invalid zero logical sector size");
        let slaves = if partition {
            BTreeSet::new()
        } else {
            list_names(&path.join("slaves"))?
        };
        let holders = list_names(&path.join("holders"))?;
        let mut connections = slaves;
        connections.extend(holders.iter().cloned());
        if let Some(parent) = &parent {
            connections.insert(parent.clone());
        }
        let serial = optional_text(&path.join("device/serial"))?;
        let (usb, serial) = usb_identity(&canonical, serial)?;
        let wwid = optional_text(&path.join("wwid"))?.or(optional_text(&path.join("device/wwid"))?);
        graph.insert(
            name.clone(),
            Node {
                name,
                dev,
                canonical,
                capacity,
                logical_sector_size,
                partition,
                parent,
                connections,
                holders,
                usb,
                removable: if partition {
                    false
                } else {
                    text(&path.join("removable"))? == "1"
                },
                read_only: text(&path.join("ro"))? == "1",
                serial,
                wwid,
                model: optional_text(&path.join("device/model"))?
                    .unwrap_or_else(|| "Unknown".into()),
                vendor: optional_text(&path.join("device/vendor"))?
                    .unwrap_or_else(|| "Unknown".into()),
            },
        );
    }
    // The kernel exposes multi-device Btrfs membership separately from the
    // ordinary block holder/slave graph. All members must be protected together.
    let btrfs = Path::new("/sys/fs/btrfs");
    if btrfs.try_exists()? {
        for entry in fs::read_dir(btrfs)? {
            let devices = entry?.path().join("devices");
            if !devices.try_exists()? {
                continue;
            }
            let mut members = Vec::new();
            for member in fs::read_dir(devices)? {
                let number = dev_number(&text(&member?.path().join("dev"))?)?;
                let name = graph
                    .values()
                    .find(|n| n.dev == number)
                    .with_context(|| {
                        format!(
                            "Btrfs member {}:{} absent from topology",
                            number.0, number.1
                        )
                    })?
                    .name
                    .clone();
                members.push(name);
            }
            ensure!(
                !members.is_empty(),
                "Btrfs filesystem has no visible device members"
            );
            connect_group(&mut graph, &members)?;
        }
    }
    let edges: Vec<_> = graph
        .values()
        .flat_map(|n| n.connections.iter().map(|to| (n.name.clone(), to.clone())))
        .collect();
    for (from, to) in edges {
        ensure!(
            graph.contains_key(&to),
            "storage graph references missing node {to}"
        );
        graph
            .get_mut(&to)
            .expect("checked above")
            .connections
            .insert(from);
    }
    Ok(graph)
}

fn usb_identity(canonical: &Path, mut serial: Option<String>) -> Result<(bool, Option<String>)> {
    let mut usb = false;
    for ancestor in canonical.ancestors() {
        if fs::read_link(ancestor.join("subsystem"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s == "usb"))
            .unwrap_or(false)
        {
            usb = true;
            // Interface nodes have no idVendor; the nearest actual USB device
            // does. Never inherit an upstream hub/controller's unrelated serial.
            if ancestor.join("idVendor").try_exists()? {
                if serial.is_none() {
                    serial = optional_text(&ancestor.join("serial"))?;
                }
                break;
            }
        }
    }
    Ok((usb, serial))
}

fn connect_group(graph: &mut Graph, names: &[String]) -> Result<()> {
    for from in names {
        let node = graph
            .get_mut(from)
            .context("missing storage group member")?;
        for to in names {
            if from != to {
                node.connections.insert(to.clone());
            }
        }
    }
    Ok(())
}

fn component(graph: &Graph, initial: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut queue = VecDeque::from([initial.to_owned()]);
    while let Some(name) = queue.pop_front() {
        if found.insert(name.clone())
            && let Some(node) = graph.get(&name)
        {
            queue.extend(node.connections.iter().cloned());
        }
    }
    found
}

/// Decode the octal escaping used by mountinfo and /proc/swaps. Invalid or NUL
/// escapes fail rather than silently referring to another path.
fn unescape(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            result.push(bytes[i]);
            i += 1;
            continue;
        }
        ensure!(i + 3 < bytes.len(), "truncated procfs escape");
        let mut byte: u16 = 0;
        for digit in &bytes[i + 1..i + 4] {
            ensure!((b'0'..=b'7').contains(digit), "invalid procfs escape");
            byte = byte * 8 + u16::from(digit - b'0');
        }
        ensure!(byte > 0 && byte <= 255, "invalid escaped byte");
        result.push(byte as u8);
        i += 4;
    }
    String::from_utf8(result).context("non-UTF8 procfs path is not supported safely")
}

fn parse_mountinfo(input: &str) -> Result<Vec<Mount>> {
    let mut mounts = Vec::new();
    for line in input.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let sep = fields
            .iter()
            .position(|s| *s == "-")
            .context("mountinfo separator missing")?;
        ensure!(
            sep >= 6 && fields.len() >= sep + 4,
            "incomplete mountinfo record"
        );
        mounts.push(Mount {
            dev: dev_number(fields[2])?,
            point: unescape(fields[4])?,
            fs: fields[sep + 1].into(),
            source: unescape(fields[sep + 2])?,
        });
    }
    ensure!(
        mounts.iter().any(|m| m.point == "/"),
        "mountinfo has no root filesystem"
    );
    Ok(mounts)
}

fn source_node<'a>(graph: &'a Graph, source: &str) -> Option<&'a Node> {
    let path = Path::new(source);
    if let Ok(metadata) = fs::metadata(path)
        && metadata.file_type().is_block_device()
    {
        let dev = (libc::major(metadata.rdev()), libc::minor(metadata.rdev()));
        return graph.values().find(|n| n.dev == dev);
    }
    // This fallback permits discovery in sandboxes with hidden /dev nodes. It
    // does not authorize a write: open_exclusive always requires a real node.
    if path.parent() == Some(Path::new("/dev")) {
        path.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| graph.get(n))
    } else {
        None
    }
}

fn mount_node<'a>(graph: &'a Graph, mount: &Mount) -> Option<&'a Node> {
    graph
        .values()
        .find(|n| n.dev == mount.dev)
        .or_else(|| source_node(graph, &mount.source))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmissionScope {
    /// Desktop discovery can establish visible hazards, not complete host admission.
    Provisional,
    /// Only the privileged helper can establish authoritative host admission.
    Authoritative,
}
impl AdmissionScope {
    fn for_euid(euid: u32) -> Self {
        if euid == 0 {
            Self::Authoritative
        } else {
            Self::Provisional
        }
    }
    fn current() -> Self {
        // SAFETY: geteuid has no arguments or mutable memory effects.
        Self::for_euid(unsafe { libc::geteuid() })
    }
}

fn namespace_uncertainty(
    scope: AdmissionScope,
    ours: std::io::Result<PathBuf>,
    init: std::io::Result<PathBuf>,
) -> Option<String> {
    match (ours, init) {
        (Ok(ours), Ok(init)) if ours == init => None,
        (Ok(_), Err(_)) if scope == AdmissionScope::Provisional => None,
        (Ok(_), Ok(_)) => Some(
            "caller mount namespace differs from PID 1; run outside private mount namespaces"
                .into(),
        ),
        (Err(error), _) => Some(format!("cannot inspect caller mount namespace: {error}")),
        (_, Err(error)) => Some(format!(
            "cannot authoritatively inspect PID 1 mount namespace: {error}"
        )),
    }
}

fn pid1_mounts(
    scope: AdmissionScope,
    content: std::io::Result<String>,
    usage: &mut Usage,
) -> Result<Vec<Mount>> {
    match content {
        Ok(value) => parse_mountinfo(&value),
        Err(_) if scope == AdmissionScope::Provisional => Ok(Vec::new()),
        Err(error) => {
            usage
                .uncertainties
                .insert(format!("cannot inspect PID 1 mount namespace: {error}"));
            Ok(Vec::new())
        }
    }
}

fn authorize_open(scope: AdmissionScope, writable: bool) -> Result<()> {
    ensure!(
        !writable || scope == AdmissionScope::Authoritative,
        "physical writes require the root helper's authoritative system-storage preflight; desktop eligibility is provisional"
    );
    Ok(())
}

fn read_usage(graph: &Graph) -> Result<Usage> {
    let mut usage = Usage::default();
    let scope = AdmissionScope::current();
    if let Some(reason) = namespace_uncertainty(
        scope,
        fs::read_link("/proc/self/ns/mnt"),
        fs::read_link("/proc/1/ns/mnt"),
    ) {
        usage.uncertainties.insert(reason);
    }
    let self_mounts = parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo")?)?;
    let host_mounts = pid1_mounts(scope, fs::read_to_string("/proc/1/mountinfo"), &mut usage)?;
    for mount in self_mounts.iter().chain(&host_mounts) {
        if mount.fs == "btrfs" && !Path::new("/sys/fs/btrfs").try_exists()? {
            usage.uncertainties.insert(
                "Btrfs is mounted but kernel multi-device membership is unavailable".into(),
            );
        }
        if let Some(node) = mount_node(graph, mount) {
            usage
                .mounts
                .entry(node.name.clone())
                .or_default()
                .insert(mount.point.clone());
            usage
                .filesystems
                .insert(node.name.clone(), mount.fs.clone());
        } else if mount.dev.0 != 0 || mount.source.starts_with("/dev/") || mount.fs == "btrfs" {
            usage.uncertainties.insert(format!(
                "cannot resolve active mount {} ({}, {}) to block topology",
                mount.point, mount.fs, mount.source
            ));
        } else if matches!(mount.fs.as_str(), "overlay" | "fuseblk" | "aufs") {
            usage.uncertainties.insert(format!(
                "unsupported block backing for {} mount {}",
                mount.fs, mount.point
            ));
        }
    }
    let swaps = fs::read_to_string("/proc/swaps").context("read active swap list")?;
    ensure!(
        swaps
            .lines()
            .next()
            .is_some_and(|h| h.starts_with("Filename")),
        "invalid swap list header"
    );
    for line in swaps.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 5, "invalid swap entry");
        let path = unescape(fields[0])?;
        let node = if fields[1] == "file" {
            // A swap file inherits every physical member of its mount, including
            // Btrfs multi-device membership joined by read_graph().
            let mount = self_mounts
                .iter()
                .chain(&host_mounts)
                .filter(|m| Path::new(&path).starts_with(&m.point))
                .max_by_key(|m| m.point.len());
            mount.and_then(|m| mount_node(graph, m))
        } else {
            source_node(graph, &path)
        };
        if let Some(node) = node {
            usage.swap.insert(node.name.clone());
        } else {
            usage
                .uncertainties
                .insert(format!("cannot resolve active swap {path}"));
        }
    }
    Ok(usage)
}

fn udev_filesystem(dev: (u32, u32)) -> Option<String> {
    let value = fs::read_to_string(format!("/run/udev/data/b{}:{}", dev.0, dev.1)).ok()?;
    value
        .lines()
        .find_map(|line| line.strip_prefix("E:ID_FS_TYPE=").map(str::to_owned))
}

fn validate_path(path: &Path) -> Result<()> {
    let parts: Vec<_> = path.components().collect();
    ensure!(
        parts.len() == 3
            && parts[0] == Component::RootDir
            && parts[1] == Component::Normal("dev".as_ref())
            && matches!(parts[2], Component::Normal(_)),
        "only direct whole-device paths /dev/<kernel-name> are accepted"
    );
    Ok(())
}

fn validate_device_node(path: &Path, dev: (u32, u32)) -> Result<()> {
    validate_path(path)?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    ensure!(
        metadata.file_type().is_block_device(),
        "path is not a block device (symlinks are rejected)"
    );
    ensure!(
        (libc::major(metadata.rdev()), libc::minor(metadata.rdev())) == dev,
        "block device number changed"
    );
    Ok(())
}

/// Require a current eligible whole USB disk with exactly the reviewed identity.
/// At nonzero effective UID this is provisional; writable block-device opens
/// require root and repeat this check with authoritative PID 1 visibility.
pub fn revalidate(expected: &Device) -> Result<()> {
    validate_path(&expected.path)?;
    let current = discover()?
        .into_iter()
        .find(|d| d.path == expected.path)
        .context("selected whole device disappeared")?;
    ensure!(
        current.identity == expected.identity,
        "device identity, capacity, or storage topology changed; create a new plan"
    );
    ensure!(
        current.model == expected.model
            && current.vendor == expected.vendor
            && current.capacity == expected.capacity,
        "device model, vendor, or capacity changed; create a new plan"
    );
    ensure!(
        current.eligible,
        "device is not eligible: {}",
        current.reasons.join("; ")
    );
    Ok(())
}

/// Claim a selected block device exclusively and revalidate after opening.
///
/// The returned descriptor owns the kernel claim and advisory lock until drop.
/// Linux's block O_EXCL claim prevents mounting/holders while this handle lives;
/// flock additionally coordinates cooperating raw writers. Another privileged
/// process doing raw I/O without respecting claims is outside that guarantee.
pub fn open_exclusive(expected: &Device, writable: bool) -> Result<File> {
    // This guard precedes discovery or any target open: disk-group membership
    // cannot bypass the helper's authoritative root-only system checks.
    authorize_open(AdmissionScope::current(), writable)?;
    revalidate(expected)?;
    validate_device_node(
        &expected.path,
        (expected.identity.major, expected.identity.minor),
    )?;
    let file = OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_EXCL)
        .open(&expected.path)
        .with_context(|| {
            format!(
                "exclusively open {}; unmount it first and authorize the scoped helper",
                expected.path.display()
            )
        })?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.file_type().is_block_device(),
        "opened descriptor is not a block device"
    );
    ensure!(
        (libc::major(metadata.rdev()), libc::minor(metadata.rdev()))
            == (expected.identity.major, expected.identity.minor),
        "opened device number differs from reviewed identity"
    );
    // SAFETY: flock only examines this valid, owned file descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("another process holds the device lock");
    }
    ensure!(
        capacity(&file)? == expected.capacity,
        "opened block-device capacity changed"
    );
    ensure!(
        logical_sector_size(&file)? == expected.identity.logical_sector_size,
        "opened block-device logical sector size changed"
    );
    revalidate(expected)?;
    Ok(file)
}

/// Query the opened Linux block device, rather than trusting a pathname or size
/// cached before open. Regular files are deliberately rejected.
pub fn capacity(file: &File) -> Result<u64> {
    ensure!(
        file.metadata()?.file_type().is_block_device(),
        "capacity ioctl requires a block device"
    );
    let mut bytes: u64 = 0;
    // Linux UAPI BLKGETSIZE64 = _IOR(0x12, 114, size_t), for the x86-64 target.
    const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
    // SAFETY: bytes points to a writable u64 for the full ioctl call.
    if unsafe { libc::ioctl(file.as_raw_fd(), BLKGETSIZE64, &mut bytes) } != 0 {
        return Err(std::io::Error::last_os_error()).context("BLKGETSIZE64 failed");
    }
    Ok(bytes)
}

/// Query Linux's logical sector size on the retained block descriptor.
pub fn logical_sector_size(file: &File) -> Result<u32> {
    ensure!(
        file.metadata()?.file_type().is_block_device(),
        "sector-size ioctl requires a block device"
    );
    let mut size: libc::c_int = 0;
    const BLKSSZGET: libc::c_ulong = 0x1268;
    // SAFETY: size points to a writable C int, as required by Linux BLKSSZGET.
    if unsafe { libc::ioctl(file.as_raw_fd(), BLKSSZGET, &mut size) } != 0 {
        return Err(std::io::Error::last_os_error()).context("BLKSSZGET failed");
    }
    ensure!(size > 0, "invalid logical block size");
    Ok(size as u32)
}

/// Mounts must be removed explicitly through the desktop's trusted UDisks flow.
/// Keeping this boundary explicit avoids unmounting root storage or silently
/// changing a plan's topology during a privileged write request.
pub fn unmount(_device: &Device) -> Result<()> {
    bail!(
        "automatic unmounting is not implemented; unmount every target partition in the desktop file manager, then refresh and review a new plan"
    )
}

/// Polling adapter for GUI/CLI hotplug monitoring. Call on a background worker;
/// every sample reconstructs device eligibility and returns changed snapshots.
#[derive(Default)]
pub struct DeviceMonitor {
    previous: Option<Vec<Device>>,
}
impl DeviceMonitor {
    pub fn sample(&mut self) -> Result<Option<Vec<Device>>> {
        let current = discover()?;
        if self.previous.as_ref() == Some(&current) {
            return Ok(None);
        }
        self.previous = Some(current.clone());
        Ok(Some(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, dev: (u32, u32)) -> Node {
        Node {
            name: name.into(),
            dev,
            canonical: Path::new("/sys/devices/test").join(name),
            capacity: 1024,
            logical_sector_size: 512,
            partition: false,
            parent: None,
            connections: BTreeSet::new(),
            holders: BTreeSet::new(),
            usb: true,
            removable: true,
            read_only: false,
            serial: Some("example".into()),
            wwid: None,
            model: "test".into(),
            vendor: "test".into(),
        }
    }

    #[test]
    fn inaccessible_pid1_is_provisional_only_for_unprivileged_discovery() {
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        for euid in [1000, 65534] {
            let scope = AdmissionScope::for_euid(euid);
            assert!(namespace_uncertainty(scope, Ok("mnt:[1]".into()), Err(denied())).is_none());
            let mut usage = Usage::default();
            assert!(
                pid1_mounts(scope, Err(denied()), &mut usage)
                    .unwrap()
                    .is_empty()
            );
            assert!(usage.uncertainties.is_empty());
        }
        let scope = AdmissionScope::for_euid(0);
        assert!(namespace_uncertainty(scope, Ok("mnt:[1]".into()), Err(denied())).is_some());
        let mut usage = Usage::default();
        assert!(
            pid1_mounts(scope, Err(denied()), &mut usage)
                .unwrap()
                .is_empty()
        );
        assert!(!usage.uncertainties.is_empty());
    }

    #[test]
    fn visible_namespace_mismatch_and_malformed_mounts_always_refuse() {
        for scope in [AdmissionScope::Provisional, AdmissionScope::Authoritative] {
            assert!(
                namespace_uncertainty(scope, Ok("mnt:[1]".into()), Ok("mnt:[2]".into())).is_some()
            );
            assert!(
                namespace_uncertainty(scope, Ok("mnt:[1]".into()), Ok("mnt:[1]".into())).is_none()
            );
            assert!(
                namespace_uncertainty(
                    scope,
                    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                    Ok("mnt:[1]".into())
                )
                .is_some()
            );
            let mut usage = Usage::default();
            assert!(pid1_mounts(scope, Ok("malformed visible mount".into()), &mut usage).is_err());
        }
    }

    #[test]
    fn physical_write_open_requires_root_authoritative_scope() {
        for euid in [1, 1000, 65534] {
            assert!(authorize_open(AdmissionScope::for_euid(euid), true).is_err());
            assert!(authorize_open(AdmissionScope::for_euid(euid), false).is_ok());
        }
        assert!(authorize_open(AdmissionScope::for_euid(0), true).is_ok());
    }

    #[test]
    fn unprivileged_writable_api_refuses_before_device_access() {
        if AdmissionScope::current() == AdmissionScope::Authoritative {
            return;
        }
        let device = Device {
            path: "/dev/__forgeusb_nonexistent_test_device__".into(),
            model: "synthetic".into(),
            vendor: "synthetic".into(),
            capacity: 0,
            identity: Identity {
                major: 0,
                minor: 0,
                sysfs_path: "/sys/devices/__forgeusb_test__".into(),
                capacity: 0,
                logical_sector_size: 512,
                serial: None,
                wwid: None,
                topology: Vec::new(),
            },
            eligible: true,
            reasons: Vec::new(),
            mounts: Vec::new(),
            partitions: Vec::new(),
            removable: true,
            transport: "usb".into(),
        };
        let error = open_exclusive(&device, true).unwrap_err();
        assert!(error.to_string().contains("root helper"));
    }

    #[test]
    fn unavailable_optional_scsi_vpd_is_not_a_topology_failure() {
        for attribute in ["device/serial", "device/wwid"] {
            let value = optional_text_value(
                Path::new(attribute),
                Err(std::io::Error::from_raw_os_error(libc::ENXIO)),
            )
            .unwrap();
            assert!(value.is_none());
        }
        assert_eq!(
            optional_text_value(Path::new("serial"), Ok("  USB-123\n".into())).unwrap(),
            Some("USB-123".into())
        );
        assert_eq!(
            optional_text_value(Path::new("serial"), Ok(" \n".into())).unwrap(),
            None
        );
    }

    #[test]
    fn unexpected_optional_identity_errors_still_fail_closed() {
        for code in [libc::EACCES, libc::EIO, libc::ENODEV, libc::EINVAL] {
            let error = optional_text_value(
                Path::new("device/serial"),
                Err(std::io::Error::from_raw_os_error(code)),
            )
            .unwrap_err();
            assert!(error.to_string().contains("device/serial"));
        }
        assert!(optional_text_value(Path::new("serial"), Ok("x".repeat(65_537))).is_err());
        let directory = tempfile::tempdir().unwrap();
        // Absence is optional, but required geometry is never optional.
        assert!(
            optional_text(&directory.path().join("missing"))
                .unwrap()
                .is_none()
        );
        assert!(text(&directory.path().join("missing")).is_err());
    }

    #[test]
    fn usb_without_serial_never_inherits_host_hub_identity() {
        let dir = tempfile::tempdir().unwrap();
        let hub = dir.path().join("hub");
        let device = hub.join("device");
        let interface = device.join("interface");
        let block = interface.join("block/sda");
        fs::create_dir_all(&block).unwrap();
        for path in [&hub, &device, &interface] {
            std::os::unix::fs::symlink("/sys/bus/usb", path.join("subsystem")).unwrap();
        }
        fs::write(hub.join("idVendor"), "1d6b").unwrap();
        fs::write(hub.join("serial"), "HOST-CONTROLLER").unwrap();
        fs::write(device.join("idVendor"), "1234").unwrap();
        assert_eq!(usb_identity(&block, None).unwrap(), (true, None));
        fs::write(device.join("serial"), "ACTUAL-USB-STICK").unwrap();
        let unavailable_scsi_serial = optional_text_value(
            Path::new("device/serial"),
            Err(std::io::Error::from_raw_os_error(libc::ENXIO)),
        )
        .unwrap();
        assert_eq!(
            usb_identity(&block, unavailable_scsi_serial).unwrap(),
            (true, Some("ACTUAL-USB-STICK".into()))
        );
        assert_eq!(
            usb_identity(&block, Some("DISK-SERIAL".into())).unwrap(),
            (true, Some("DISK-SERIAL".into()))
        );
    }

    #[test]
    fn mountinfo_octal_paths_and_btrfs_anonymous_device() {
        let mounts = parse_mountinfo("1 0 0:35 /@ / rw - btrfs /dev/nvme0n1p2 rw\n2 1 8:1 / /media/My\\040USB rw - vfat /dev/sda1 rw\n").unwrap();
        assert_eq!(mounts[1].point, "/media/My USB");
        let graph = BTreeMap::from([("nvme0n1p2".into(), node("nvme0n1p2", (259, 2)))]);
        assert_eq!(mount_node(&graph, &mounts[0]).unwrap().name, "nvme0n1p2");
    }

    #[test]
    fn malformed_procfs_is_rejected() {
        assert!(parse_mountinfo("1 0 8:1 / / rw ext4 /dev/sda1").is_err());
        assert!(parse_mountinfo("1 0 8:1 / /mnt rw - ext4 /dev/sda1 rw").is_err());
        for value in [r"/a\04", r"/a\999", r"/a\000", r"/a\777"] {
            assert!(unescape(value).is_err());
        }
    }

    #[test]
    fn graph_follows_partitions_encryption_lvm_md_and_btrfs_members() {
        let names = ["sda", "sda1", "dm-0", "dm-1", "md0", "sdb1", "sdb"];
        let mut graph: Graph = names
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).into(), node(n, (8, i as u32))))
            .collect();
        for pair in names.windows(2) {
            connect_group(&mut graph, &[pair[0].into(), pair[1].into()]).unwrap();
        }
        assert_eq!(component(&graph, "sda").len(), names.len());
        assert!(component(&graph, "sda").contains("sdb"));
        graph.insert("sdc".into(), node("sdc", (8, 99)));
        assert!(!component(&graph, "sda").contains("sdc"));
    }

    #[test]
    fn admission_protects_encrypted_root_siblings_and_swap_members() {
        let mut disk = node("sda", (8, 0));
        disk.connections.insert("sda1".into());
        let mut partition = node("sda1", (8, 1));
        partition.partition = true;
        partition.connections = BTreeSet::from(["sda".into(), "dm-0".into()]);
        let mut crypt = node("dm-0", (253, 0));
        crypt.connections.insert("sda1".into());
        let graph = BTreeMap::from([
            ("sda".into(), disk),
            ("sda1".into(), partition),
            ("dm-0".into(), crypt),
        ]);
        let usage = Usage {
            mounts: BTreeMap::from([("dm-0".into(), BTreeSet::from(["/".into(), "/home".into()]))]),
            swap: BTreeSet::from(["dm-0".into()]),
            ..Usage::default()
        };
        let (reasons, mounts) = admission(&graph, &graph["sda"], &usage);
        assert!(reasons.iter().any(|r| r.contains("mounted filesystems")));
        assert!(reasons.iter().any(|r| r.contains("active swap")));
        assert!(mounts.contains("/") && mounts.contains("/home"));
    }

    #[test]
    fn admission_requires_usb_stable_identity_and_complete_usage() {
        let mut disk = node("sda", (8, 0));
        let mut graph = BTreeMap::from([("sda".into(), disk.clone())]);
        assert!(admission(&graph, &disk, &Usage::default()).0.is_empty());
        disk.logical_sector_size = 4096;
        assert!(
            admission(&graph, &disk, &Usage::default())
                .0
                .iter()
                .any(|r| r.contains("logical sectors"))
        );
        disk.logical_sector_size = 512;
        disk.usb = false;
        assert!(
            admission(&graph, &disk, &Usage::default())
                .0
                .iter()
                .any(|r| r.contains("not connected through USB"))
        );
        disk.usb = true;
        disk.serial = None;
        assert!(
            admission(&graph, &disk, &Usage::default())
                .0
                .iter()
                .any(|r| r.contains("stable serial"))
        );
        disk.serial = Some("example".into());
        graph.insert("sdb".into(), node("sdb", (8, 16)));
        assert!(
            admission(&graph, &disk, &Usage::default())
                .0
                .iter()
                .any(|r| r.contains("duplicated"))
        );
        let usage = Usage {
            uncertainties: BTreeSet::from(["unresolved root storage".into()]),
            ..Usage::default()
        };
        assert!(
            admission(&graph, &disk, &usage)
                .0
                .contains("unresolved root storage")
        );
    }

    #[test]
    fn direct_paths_only_and_regular_files_refused() {
        for path in [
            "/tmp/disk",
            "/dev/disk/by-id/usb-a",
            "/dev/../etc/passwd",
            "/dev",
            "sda",
        ] {
            assert!(validate_path(Path::new(path)).is_err(), "{path}");
        }
        assert!(validate_path(Path::new("/dev/sda")).is_ok());
        let file = tempfile::tempfile().unwrap();
        assert!(capacity(&file).is_err());
        assert!(logical_sector_size(&file).is_err());
        assert!(validate_device_node(Path::new("/dev/null"), (1, 3)).is_err());
    }

    #[test]
    fn device_numbers_and_capacity_overflow_are_checked() {
        assert_eq!(dev_number("259:123").unwrap(), (259, 123));
        assert!(dev_number("259:-1").is_err());
        assert!(dev_number("259:2:3").is_err());
        assert!(u64::MAX.checked_mul(512).is_none());
    }
}
