//! Plain-text renderings for terminal users. `--json` keeps machine output.
use forgeusb_device::Device;
use forgeusb_image::{ImageInfo, ImageKind};
use forgeusb_linux::{Plan, Report, TargetKind};

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Marketing-style decimal size used on drive labels ("64 GB").
pub fn drive_size(n: u64) -> String {
    let gb = n as f64 / 1e9;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

pub fn device_name(d: &Device) -> String {
    let model = [d.vendor.trim(), d.model.trim()]
        .into_iter()
        .filter(|s| !s.is_empty() && *s != "Unknown")
        .collect::<Vec<_>>()
        .join(" ");
    let model = if model.is_empty() {
        "Storage device".into()
    } else {
        model
    };
    format!("{model} ({})", drive_size(d.capacity))
}

pub fn devices(list: &[Device]) -> String {
    if list.is_empty() {
        return "No disks found.\n".into();
    }
    let mut out = String::new();
    for d in list {
        let state = if d.eligible {
            "ready"
        } else if d.blocked_only_by_mounts {
            "mounted (run: forgeusb unmount)"
        } else {
            "not allowed"
        };
        out += &format!(
            "{:<14} {:<40} {:<5} {}\n",
            d.path.display(),
            device_name(d),
            d.transport,
            state
        );
        for p in &d.partitions {
            let fs = p.filesystem.as_deref().unwrap_or("unknown");
            let label = p
                .label
                .as_deref()
                .map(|l| format!(" \"{l}\""))
                .unwrap_or_default();
            let mounted = if p.mounts.is_empty() {
                String::new()
            } else {
                format!("  mounted at {}", p.mounts.join(", "))
            };
            out += &format!(
                "  └ {} {} {fs}{label}{mounted}\n",
                p.path.display(),
                bytes(p.capacity)
            );
        }
        if !d.eligible && !d.blocked_only_by_mounts && d.transport == "usb" {
            for r in &d.reasons {
                out += &format!("    ! {r}\n");
            }
        }
    }
    out
}

pub fn kind(k: &ImageKind) -> &'static str {
    match k {
        ImageKind::Disk => "disk image",
        ImageKind::IsoHybrid => "hybrid ISO (writable as a disk image)",
        ImageKind::OpticalIso => "optical ISO",
        ImageKind::Udf => "UDF image",
        ImageKind::Compressed => "compressed image",
        ImageKind::Unknown => "unrecognized",
    }
}

pub fn image(i: &ImageInfo) -> String {
    let mut out = format!(
        "Image:    {}\nType:     {}{}\nSize:     {} ({} bytes)\nSHA-256:  {}\n",
        i.path.display(),
        kind(&i.kind),
        if i.windows { ", Windows installer" } else { "" },
        bytes(i.size),
        i.size,
        i.sha256
    );
    if i.coverage == forgeusb_image::Coverage::DataRanges {
        out += &format!(
            "Coverage: prepared image, {} of data (digest covers data ranges)\n",
            bytes(i.transfer_bytes())
        );
    }
    let next = if i.windows {
        "Next:     forgeusb prepare-windows IMAGE --device /dev/sdX --output NEW.img"
    } else if i.kind == ImageKind::Compressed {
        "Next:     forgeusb decompress IMAGE --output NEW.img"
    } else if i.bootable_disk {
        "Next:     forgeusb plan IMAGE --device /dev/sdX"
    } else {
        "Next:     this image cannot be written as-is"
    };
    out += next;
    out.push('\n');
    for w in &i.warnings {
        out += &format!("Note:     {w}\n");
    }
    out
}

pub fn plan(p: &Plan) -> String {
    let target = match &p.target_kind {
        TargetKind::Block(d) => format!("{} — {}", p.target.display(), device_name(d)),
        TargetKind::TestFile => format!("{} (new disposable file)", p.target.display()),
    };
    let mut out = format!(
        "Source:   {} ({})\nTarget:   {target}\nMethod:   {}\nWrites:   {}\nSHA-256:  {}\n",
        p.image.path.display(),
        bytes(p.image.size),
        p.strategy,
        bytes(p.image.transfer_bytes()),
        p.image.sha256
    );
    if let TargetKind::Block(d) = &p.target_kind {
        for part in &d.partitions {
            out += &format!(
                "Erases:   {} {}{}\n",
                part.path.display(),
                part.filesystem.as_deref().unwrap_or("data"),
                part.label
                    .as_deref()
                    .map(|l| format!(" \"{l}\""))
                    .unwrap_or_default()
            );
        }
    }
    for w in &p.warnings {
        out += &format!("Note:     {w}\n");
    }
    out += &format!("Confirm:  {}\n", p.confirmation);
    out
}

pub fn report(r: &Report) -> String {
    // Verification-only reports write nothing (`written` is 0).
    let written = if r.written == 0 {
        String::new()
    } else {
        format!(" ({} written)", bytes(r.written))
    };
    let mut out = format!(
        "Verified: {}{written}, SHA-256 {}\n",
        bytes(r.bytes),
        r.sha256
    );
    for n in &r.notes {
        out += &format!("Note:     {n}\n");
    }
    out
}

pub fn dependencies(v: &serde_json::Value) -> String {
    let mut out = String::new();
    for item in v.as_array().into_iter().flatten() {
        let name = item["name"].as_str().unwrap_or("?");
        let path = item["path"].as_str().unwrap_or("");
        let mark = if item["installed"].as_bool() == Some(true) {
            "ok     "
        } else {
            "missing"
        };
        out += &format!("{mark} {name:<16} {path}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes_read_naturally() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(drive_size(62_277_025_792), "62 GB");
        assert_eq!(drive_size(8_000_000_000), "8.0 GB");
    }
}
