//! Partition planning and ordinary-file table construction. Never opens a device.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

pub const ALIGNMENT: u64 = 1024 * 1024;
pub const ESP_TYPE_GUID: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Mbr,
    Gpt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub start_lba: u64,
    pub sectors: u64,
    pub filesystem: String,
    pub partition_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub scheme: Scheme,
    pub sector_bytes: u64,
    pub capacity_bytes: u64,
    pub partitions: Vec<Partition>,
    pub explanation: String,
}

/// Plan a single FAT32 EFI System Partition, leaving primary/backup GPT space.
/// This is a geometry plan, not a filesystem formatter or firmware guarantee.
pub fn plan_uefi_fat32(capacity: u64, sector: u64, scheme: Scheme) -> Result<Layout> {
    ensure!(
        matches!(sector, 512 | 4096),
        "supported logical sector sizes: 512 or 4096 bytes"
    );
    ensure!(
        capacity.is_multiple_of(sector),
        "capacity must contain whole logical sectors"
    );
    let total = capacity / sector;
    let entries = 16_384_u64.div_ceil(sector);
    let reserve_end = if scheme == Scheme::Gpt {
        entries + 1
    } else {
        0
    };
    let first_usable = if scheme == Scheme::Gpt {
        entries + 2
    } else {
        1
    };
    let alignment = ALIGNMENT / sector;
    let start = first_usable
        .checked_add(alignment - 1)
        .ok_or_else(|| anyhow::anyhow!("alignment overflow"))?
        / alignment
        * alignment;
    let usable_end = total
        .checked_sub(reserve_end)
        .ok_or_else(|| anyhow::anyhow!("device too small for partition metadata"))?;
    let sectors = usable_end
        .checked_sub(start)
        .ok_or_else(|| anyhow::anyhow!("device too small for aligned partition"))?;
    ensure!(
        sectors
            .checked_mul(sector)
            .is_some_and(|v| v >= 260 * ALIGNMENT),
        "FAT32 EFI policy requires at least 260 MiB after alignment and metadata"
    );
    ensure!(
        sectors <= u64::from(u32::MAX),
        "FAT32 BPB sector-count field would overflow; a smaller partition or different filesystem is required"
    );
    if scheme == Scheme::Mbr
        && (start > u32::MAX.into() || sectors > u32::MAX.into() || total > u64::from(u32::MAX) + 1)
    {
        bail!("MBR address range exceeded; choose GPT");
    }
    Ok(Layout {
        scheme,
        sector_bytes: sector,
        capacity_bytes: capacity,
        partitions: vec![Partition {
            start_lba: start,
            sectors,
            filesystem: "fat32".into(),
            partition_type: if scheme == Scheme::Gpt {
                ESP_TYPE_GUID.into()
            } else {
                "0xef".into()
            },
        }],
        explanation: "One 1 MiB-aligned FAT32 EFI System Partition; UEFI media, no BIOS boot code"
            .into(),
    })
}

/// Plan one ordinary NTFS data partition; this does not install a boot loader.
pub fn plan_ntfs(capacity: u64, sector: u64, scheme: Scheme) -> Result<Layout> {
    let mut layout = plan_uefi_fat32(capacity, sector, scheme)?;
    layout.partitions[0].filesystem = "ntfs".into();
    layout.partitions[0].partition_type = if scheme == Scheme::Gpt {
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7".into()
    } else {
        "0x07".into()
    };
    layout.explanation =
        "One aligned NTFS data partition; no UEFI driver or BIOS bootstrap is installed".into();
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpt_512_reserves_backup() {
        let l = plan_uefi_fat32(1024 * ALIGNMENT, 512, Scheme::Gpt).unwrap();
        let p = &l.partitions[0];
        assert_eq!(p.start_lba, 2048);
        assert_eq!(p.start_lba + p.sectors, 1024 * ALIGNMENT / 512 - 33);
        assert_eq!(p.partition_type, ESP_TYPE_GUID);
    }
    #[test]
    fn gpt_4k_reserves_exact_entry_array() {
        let l = plan_uefi_fat32(1024 * ALIGNMENT, 4096, Scheme::Gpt).unwrap();
        assert_eq!(l.partitions[0].start_lba, 256);
        assert_eq!(
            l.partitions[0].start_lba + l.partitions[0].sectors,
            1024 * ALIGNMENT / 4096 - 5
        );
    }
    #[test]
    fn rejects_invalid_geometry() {
        assert!(plan_uefi_fat32(u64::MAX, 512, Scheme::Gpt).is_err());
        assert!(plan_uefi_fat32(ALIGNMENT, 512, Scheme::Gpt).is_err());
        assert!(plan_uefi_fat32(1024 * ALIGNMENT, 520, Scheme::Gpt).is_err());
        assert!(plan_uefi_fat32(3 * 1024 * 1024 * ALIGNMENT, 512, Scheme::Mbr).is_err());
        assert!(plan_uefi_fat32(4 * 1024 * 1024 * ALIGNMENT, 512, Scheme::Gpt).is_err());
    }
}

mod table;
pub use table::{verify_partition_table, write_partition_table};
