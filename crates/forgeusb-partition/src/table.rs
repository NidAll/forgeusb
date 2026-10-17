//! Minimal MBR/GPT serialization for new, retained regular-file disk images.
use crate::{Layout, Scheme, plan_ntfs, plan_uefi_fat32};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn get64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
const ESP: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const BASIC_DATA: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];
fn type_guid(layout: &Layout) -> [u8; 16] {
    if layout.partitions[0].filesystem == "ntfs" {
        BASIC_DATA
    } else {
        ESP
    }
}
fn geometry(file: &File, layout: &Layout) -> Result<()> {
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.uid() == unsafe { libc::geteuid() } && m.nlink() <= 1,
        "partition output must be a retained caller-owned regular file with at most one link"
    );
    ensure!(
        m.len() == layout.capacity_bytes,
        "partition output size differs from layout"
    );
    let planned = if layout
        .partitions
        .first()
        .is_some_and(|p| p.filesystem == "ntfs")
    {
        plan_ntfs(layout.capacity_bytes, layout.sector_bytes, layout.scheme)?
    } else {
        plan_uefi_fat32(layout.capacity_bytes, layout.sector_bytes, layout.scheme)?
    };
    ensure!(
        layout.partitions == planned.partitions,
        "unsupported or modified partition geometry"
    );
    Ok(())
}
fn at(file: &mut File, offset: u64, bytes: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    Ok(())
}
fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut b = vec![0; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut b)?;
    Ok(b)
}
fn mbr(layout: &Layout) -> Vec<u8> {
    let mut b = vec![0; 512];
    let p = &layout.partitions[0];
    b[447..450].copy_from_slice(&[0xfe, 0xff, 0xff]);
    b[451..454].copy_from_slice(&[0xfe, 0xff, 0xff]);
    b[450] = if layout.scheme == Scheme::Gpt {
        0xee
    } else if p.filesystem == "ntfs" {
        0x07
    } else {
        0xef
    };
    put32(
        &mut b,
        454,
        if layout.scheme == Scheme::Gpt {
            1
        } else {
            p.start_lba as u32
        },
    );
    put32(
        &mut b,
        458,
        if layout.scheme == Scheme::Gpt {
            (layout.capacity_bytes / layout.sector_bytes - 1).min(u32::MAX.into()) as u32
        } else {
            p.sectors as u32
        },
    );
    b[510] = 0x55;
    b[511] = 0xaa;
    b
}
/// Write a table into a retained ordinary file; no path/device is opened here.
/// The generated MBR has no BIOS bootstrap. GPT has an ESP and redundant CRCs.
pub fn write_partition_table(file: &mut File, layout: &Layout) -> Result<()> {
    geometry(file, layout)?;
    at(file, 0, &mbr(layout))?;
    if layout.scheme == Scheme::Gpt {
        let sector = layout.sector_bytes;
        let last = layout.capacity_bytes / sector - 1;
        let count = 16384u64.div_ceil(sector);
        let mut ids = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut ids)?;
        let p = &layout.partitions[0];
        let mut entries = vec![0; 16384];
        entries[..16].copy_from_slice(&type_guid(layout));
        entries[16..32].copy_from_slice(&ids[16..]);
        put64(&mut entries, 32, p.start_lba);
        put64(&mut entries, 40, p.start_lba + p.sectors - 1);
        for (i, c) in "ForgeUSB".encode_utf16().enumerate() {
            entries[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        let entries_crc = crc32(&entries);
        for (current, backup, entry_lba) in [(1, last, 2), (last, 1, last - count)] {
            let mut header = vec![0; sector as usize];
            header[..8].copy_from_slice(b"EFI PART");
            put32(&mut header, 8, 0x10000);
            put32(&mut header, 12, 92);
            put64(&mut header, 24, current);
            put64(&mut header, 32, backup);
            put64(&mut header, 40, count + 2);
            put64(&mut header, 48, last - count - 1);
            header[56..72].copy_from_slice(&ids[..16]);
            put64(&mut header, 72, entry_lba);
            put32(&mut header, 80, 128);
            put32(&mut header, 84, 128);
            put32(&mut header, 88, entries_crc);
            let crc = crc32(&header[..92]);
            put32(&mut header, 16, crc);
            at(file, current * sector, &header)?;
            at(file, entry_lba * sector, &entries)?;
        }
    }
    file.sync_all()?;
    verify_partition_table(file, layout)
}
/// Read back all planned partition metadata, including both GPT copies/CRCs.
pub fn verify_partition_table(file: &mut File, layout: &Layout) -> Result<()> {
    geometry(file, layout)?;
    ensure!(
        read_at(file, 0, 512)? == mbr(layout),
        "MBR read-back mismatch"
    );
    if layout.scheme == Scheme::Gpt {
        let sector = layout.sector_bytes;
        let last = layout.capacity_bytes / sector - 1;
        let count = 16384u64.div_ceil(sector);
        let mut disk_id = None;
        let mut previous_entries = None;
        for (current, backup, entry_lba) in [(1, last, 2), (last, 1, last - count)] {
            let mut h = read_at(file, current * sector, sector as usize)?;
            ensure!(
                &h[..8] == b"EFI PART"
                    && get32(&h, 8) == 0x10000
                    && get32(&h, 12) == 92
                    && get32(&h, 20) == 0,
                "invalid GPT header"
            );
            let crc = get32(&h, 16);
            put32(&mut h, 16, 0);
            ensure!(crc32(&h[..92]) == crc, "GPT header CRC mismatch");
            ensure!(
                get64(&h, 24) == current
                    && get64(&h, 32) == backup
                    && get64(&h, 40) == count + 2
                    && get64(&h, 48) == last - count - 1,
                "GPT geometry mismatch"
            );
            ensure!(
                get64(&h, 72) == entry_lba && get32(&h, 80) == 128 && get32(&h, 84) == 128,
                "GPT entry geometry mismatch"
            );
            let e = read_at(file, entry_lba * sector, 16384)?;
            ensure!(crc32(&e) == get32(&h, 88), "GPT entries CRC mismatch");
            let p = layout
                .partitions
                .first()
                .context("missing planned partition")?;
            ensure!(
                e[..16] == type_guid(layout)
                    && get64(&e, 32) == p.start_lba
                    && get64(&e, 40) == p.start_lba + p.sectors - 1
                    && e[128..].iter().all(|b| *b == 0),
                "GPT partition differs from plan"
            );
            if let Some(id) = &disk_id {
                ensure!(&h[56..72] == id, "GPT disk GUID mismatch");
            } else {
                disk_id = Some(h[56..72].to_vec());
            }
            if let Some(old) = &previous_entries {
                ensure!(&e == old, "GPT backup partition entries differ");
            } else {
                previous_entries = Some(e);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_roundtrip_and_corruption() {
        for sector in [512, 4096] {
            for scheme in [Scheme::Mbr, Scheme::Gpt] {
                let mut file = tempfile::tempfile().unwrap();
                let l = plan_uefi_fat32(320 * 1024 * 1024, sector, scheme).unwrap();
                file.set_len(l.capacity_bytes).unwrap();
                write_partition_table(&mut file, &l).unwrap();
                verify_partition_table(&mut file, &l).unwrap();
                at(
                    &mut file,
                    if scheme == Scheme::Gpt {
                        sector + 16
                    } else {
                        450
                    },
                    &[0],
                )
                .unwrap();
                assert!(verify_partition_table(&mut file, &l).is_err());
            }
        }
    }
    #[test]
    fn crc_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
    }
}
