//! Bounded structural inspection. File suffixes never select a write strategy.
use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::Cancellation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStamp {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_ns: i64,
    pub ctime: i64,
    pub ctime_ns: i64,
}
impl FileStamp {
    pub fn of(f: &File) -> Result<Self> {
        let m = f.metadata()?;
        ensure!(m.is_file(), "source must be a regular file");
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    Disk,
    IsoHybrid,
    OpticalIso,
    Udf,
    Unknown,
    Compressed,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub kind: ImageKind,
    pub bootable_disk: bool,
    pub iso9660: bool,
    pub udf: bool,
    pub el_torito: bool,
    pub windows: bool,
    pub warnings: Vec<String>,
    pub stamp: FileStamp,
}
pub fn open_source(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open source {} (symlinks rejected)", path.display()))?;
    FileStamp::of(&file)?;
    Ok(file)
}
pub fn inspect(path: &Path) -> Result<ImageInfo> {
    inspect_cancel(path, &Cancellation::default())
}
pub fn inspect_cancel(path: &Path, cancel: &Cancellation) -> Result<ImageInfo> {
    let mut f = open_source(path)?;
    inspect_opened(path, &mut f, cancel)
}
/// Inspect an already opened regular file. The helper retains this descriptor
/// from an unprivileged open through writing; path replacement cannot redirect reads.
pub fn inspect_opened(path: &Path, f: &mut File, cancel: &Cancellation) -> Result<ImageInfo> {
    let stamp = FileStamp::of(f)?;
    f.seek(SeekFrom::Start(0))?;
    ensure!(stamp.size >= 512, "image is smaller than one disk sector");
    let mut head = [0u8; 512];
    f.read_exact(&mut head)?;
    let compressed = head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0])
        || head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || head.starts_with(b"BZh");
    let mut iso = false;
    let mut udf = false;
    let mut torito = false;
    let mut windows = false;
    let mut warnings = Vec::new();
    for sector in 16..=271u64 {
        cancel.check()?;
        let offset = sector * 2048;
        if offset + 2048 > stamp.size {
            break;
        }
        f.seek(SeekFrom::Start(offset))?;
        let mut d = [0u8; 2048];
        f.read_exact(&mut d)?;
        if &d[1..6] == b"CD001" {
            ensure!(d[6] == 1, "unsupported ISO descriptor version");
            if d[0] == 1 {
                let blocks = le32(&d[80..84]) as u64;
                ensure!(
                    blocks == u32::from_be_bytes(d[84..88].try_into()?) as u64,
                    "ISO volume size endian mismatch"
                );
                ensure!(
                    u16::from_le_bytes(d[128..130].try_into()?) == 2048
                        && u16::from_be_bytes(d[130..132].try_into()?) == 2048,
                    "unsupported ISO logical block size"
                );
                ensure!(
                    blocks > sector && blocks.checked_mul(2048).is_some_and(|n| n <= stamp.size),
                    "truncated ISO volume"
                );
                iso = true;
                windows = iso_has_windows(f, &d[156..190], blocks * 2048)?;
            }
            if d[0] == 0 && d[7..39].starts_with(b"EL TORITO SPECIFICATION") {
                let off = (le32(&d[71..75]) as u64)
                    .checked_mul(2048)
                    .context("El Torito offset overflow")?;
                if off + 64 <= stamp.size {
                    let mut cat = [0u8; 64];
                    f.seek(SeekFrom::Start(off))?;
                    f.read_exact(&mut cat)?;
                    let sum = cat[..32].as_chunks::<2>().0.iter().fold(0u16, |s, v| {
                        s.wrapping_add(u16::from_le_bytes([v[0], v[1]]))
                    });
                    torito =
                        cat[0] == 1 && cat[30..32] == [0x55, 0xaa] && sum == 0 && cat[32] == 0x88;
                }
            }
        }
        if &d[1..6] == b"NSR02" || &d[1..6] == b"NSR03" {
            udf = true;
        }
    }
    // ISOHybrid permits deliberately overlapping/enclosing partitions. Only an
    // actual ISO volume descriptor grants these exceptions, never a filename.
    let mut disk = valid_disk(f, &head, stamp.size, iso)?;
    let starts_at_zero = head[446..510]
        .as_chunks::<16>()
        .0
        .iter()
        .any(|e| e[4] != 0 && le32(&e[8..12]) == 0);
    // ISOHybrid's enclosing MBR partition may deliberately begin at LBA 0.
    // Admit that convention only alongside an actual ISO9660 volume descriptor.
    if starts_at_zero && !iso {
        disk = false;
    }
    if !disk {
        warnings.push("No validated disk partition layout: optical installation media requires an extraction strategy.".into());
    }
    if compressed {
        warnings.push("Compressed images must be decompressed and inspected separately; direct compressed writes are refused.".into());
    }
    let kind = if compressed {
        ImageKind::Compressed
    } else if disk && iso {
        ImageKind::IsoHybrid
    } else if disk {
        ImageKind::Disk
    } else if iso {
        ImageKind::OpticalIso
    } else if udf {
        ImageKind::Udf
    } else {
        ImageKind::Unknown
    };
    f.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).context("size overflow")?;
        ensure!(total <= stamp.size, "source grew while hashing");
        hash.update(&buf[..n]);
    }
    ensure!(
        total == stamp.size && stamp == FileStamp::of(f)?,
        "source changed during inspection"
    );
    Ok(ImageInfo {
        path: path.to_path_buf(),
        size: stamp.size,
        sha256: format!("{:x}", hash.finalize()),
        kind,
        bootable_disk: disk && !compressed,
        iso9660: iso,
        udf,
        el_torito: torito,
        windows,
        warnings,
        stamp,
    })
}
fn le32(s: &[u8]) -> u32 {
    u32::from_le_bytes(s.try_into().expect("fixed-size parser slice"))
}
fn valid_disk(f: &mut File, h: &[u8; 512], size: u64, hybrid: bool) -> Result<bool> {
    if h[510..512] != [0x55, 0xaa] {
        return Ok(false);
    }
    let mut entries = 0;
    let mut protective = false;
    let mut extents = Vec::new();
    for e in h[446..510].as_chunks::<16>().0.iter() {
        if e[4] == 0 {
            continue;
        }
        entries += 1;
        if e[0] != 0 && e[0] != 0x80 {
            return Ok(false);
        }
        let start = le32(&e[8..12]) as u64;
        let len = le32(&e[12..16]) as u64;
        if len == 0 {
            return Ok(false);
        }
        if e[4] == 0xee {
            if start != 1 {
                return Ok(false);
            }
            protective = true;
            continue;
        }
        extents.push((start, start + len));
        if start
            .checked_add(len)
            .and_then(|n| n.checked_mul(512))
            .is_none_or(|end| end > size)
        {
            return Ok(false);
        }
    }
    if !hybrid {
        extents.sort_unstable();
        if extents.windows(2).any(|w| w[0].1 > w[1].0) {
            return Ok(false);
        }
    }
    // Some hybrid builders expose GPT using a non-protective MBR partition.
    // Inspect it whenever present rather than silently ignoring a stale GPT.
    let mut signature = [0u8; 8];
    if size >= 1024 {
        f.seek(SeekFrom::Start(512))?;
        f.read_exact(&mut signature)?;
    }
    if protective || &signature == b"EFI PART" {
        validate_gpt(f, size, hybrid)?;
    }
    Ok(entries > 0)
}
fn u64_at(h: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(h[offset..offset + 8].try_into().expect("GPT fixed slice"))
}
fn read_gpt_header(f: &mut File, lba: u64) -> Result<[u8; 512]> {
    f.seek(SeekFrom::Start(
        lba.checked_mul(512).context("GPT offset overflow")?,
    ))?;
    let mut h = [0u8; 512];
    f.read_exact(&mut h)?;
    ensure!(&h[..8] == b"EFI PART", "missing GPT header");
    ensure!(le32(&h[8..12]) == 0x0001_0000, "unsupported GPT revision");
    let len = le32(&h[12..16]) as usize;
    ensure!((92..=512).contains(&len), "invalid GPT header size");
    ensure!(h[20..24] == [0; 4], "nonzero reserved GPT field");
    let crc = le32(&h[16..20]);
    h[16..20].fill(0);
    ensure!(crc32fast::hash(&h[..len]) == crc, "GPT header CRC mismatch");
    ensure!(u64_at(&h, 24) == lba, "GPT header location mismatch");
    Ok(h)
}
fn validate_gpt(f: &mut File, size: u64, hybrid: bool) -> Result<()> {
    let h = read_gpt_header(f, 1)?;
    let backup = u64_at(&h, 32);
    let first_usable = u64_at(&h, 40);
    let last_usable = u64_at(&h, 48);
    ensure!(
        backup > 1
            && backup < size / 512
            && first_usable <= last_usable
            && last_usable < size / 512,
        "invalid GPT geometry"
    );
    let count = le32(&h[80..84]) as u64;
    let entry = le32(&h[84..88]) as u64;
    ensure!(
        (128..=4096).contains(&entry) && entry.is_multiple_of(8),
        "unsupported GPT entry size"
    );
    let bytes = count.checked_mul(entry).context("GPT table overflow")?;
    ensure!(
        bytes > 0 && bytes <= 1024 * 1024,
        "GPT table exceeds 1 MiB parser limit"
    );
    let table_lba = u64_at(&h, 72);
    let off = table_lba.checked_mul(512).context("GPT offset overflow")?;
    ensure!(
        off >= 1024 && off.checked_add(bytes).is_some_and(|v| v <= size),
        "GPT table outside image"
    );
    let mut table = vec![0u8; bytes as usize];
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(&mut table)?;
    ensure!(
        crc32fast::hash(&table) == le32(&h[88..92]),
        "GPT table CRC mismatch"
    );
    let mut extents = Vec::new();
    let mut guids = std::collections::BTreeSet::new();
    for e in table.chunks_exact(entry as usize) {
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        ensure!(
            first <= last
                && last < size / 512
                && (hybrid || (first >= first_usable && last <= last_usable)),
            "GPT partition outside usable range"
        );
        if !hybrid {
            ensure!(
                e[16..32].iter().any(|&b| b != 0) && guids.insert(e[16..32].to_vec()),
                "missing or duplicate GPT partition GUID"
            );
        }
        extents.push((first, last));
    }
    if !hybrid {
        // UEFI reserves at least 16 KiB for each entry array. The primary and
        // backup metadata may never occupy usable partition space.
        let table_sectors = bytes.div_ceil(512).max(32);
        ensure!(
            size.is_multiple_of(512) && backup == size / 512 - 1,
            "GPT backup is not at the image end"
        );
        ensure!(
            table_lba
                .checked_add(table_sectors)
                .is_some_and(|end| end <= first_usable)
                && last_usable < backup,
            "GPT metadata overlaps usable space"
        );
        extents.sort_unstable();
        ensure!(
            !extents.windows(2).any(|w| w[0].1 >= w[1].0),
            "overlapping GPT partitions"
        );
        let b = read_gpt_header(f, backup)?;
        ensure!(
            u64_at(&b, 32) == 1
                && b[40..72] == h[40..72]
                && b[80..92] == h[80..92]
                && b[8..16] == h[8..16],
            "GPT backup disagrees with primary header"
        );
        let backup_table = u64_at(&b, 72);
        ensure!(
            backup_table > last_usable
                && backup_table
                    .checked_add(table_sectors)
                    .is_some_and(|end| end <= backup),
            "GPT backup table overlaps usable space or header"
        );
        let mut copy = vec![0u8; bytes as usize];
        f.seek(SeekFrom::Start(
            backup_table
                .checked_mul(512)
                .context("GPT backup offset overflow")?,
        ))?;
        f.read_exact(&mut copy)?;
        ensure!(copy == table, "GPT backup entry array differs from primary");
    }
    Ok(())
}
fn iso_has_windows(f: &mut File, root: &[u8], size: u64) -> Result<bool> {
    if root.len() < 34 || root[0] < 34 {
        bail!("invalid ISO root directory");
    }
    let extent = le32(&root[2..6]);
    let len = le32(&root[10..14]) as u64;
    ensure!(
        extent == u32::from_be_bytes(root[6..10].try_into()?)
            && len == u32::from_be_bytes(root[14..18].try_into()?) as u64,
        "ISO root directory endian mismatch"
    );
    ensure!(
        root[25] & 2 != 0 && root[32..34] == [1, 0] && len > 0,
        "invalid ISO root directory identity"
    );
    let offset = (extent as u64) * 2048;
    ensure!(
        len <= 4 * 1024 * 1024 && offset.checked_add(len).is_some_and(|n| n <= size),
        "ISO root exceeds parser bounds"
    );
    let mut bytes = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(&mut bytes)?;
    let mut at = 0;
    let mut sources = false;
    let mut bootmgr = false;
    while at < bytes.len() {
        let n = bytes[at] as usize;
        if n == 0 {
            at = (at / 2048 + 1) * 2048;
            continue;
        }
        ensure!(
            n >= 34 && at + n <= bytes.len() && at % 2048 + n <= 2048,
            "malformed ISO directory record"
        );
        let r = &bytes[at..at + n];
        let namesize = r[32] as usize;
        ensure!(33 + namesize <= r.len(), "malformed ISO filename");
        let name = String::from_utf8_lossy(&r[33..33 + namesize]).to_ascii_uppercase();
        sources |= name == "SOURCES" && r[25] & 2 != 0;
        bootmgr |= matches!(name.as_str(), "BOOTMGR" | "BOOTMGR;1") && r[25] & 2 == 0;
        at += n;
    }
    Ok(sources && bootmgr)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn fixture() -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut b = vec![0u8; 65536];
        b[510] = 0x55;
        b[511] = 0xaa;
        b[446 + 4] = 0x83;
        b[454..458].copy_from_slice(&1u32.to_le_bytes());
        b[458..462].copy_from_slice(&127u32.to_le_bytes());
        f.write_all(&b).unwrap();
        f
    }
    fn gpt_fixture() -> Vec<u8> {
        let mut b = vec![0; 256 * 512];
        b[510..512].copy_from_slice(&[0x55, 0xaa]);
        b[450] = 0xee;
        b[454..458].copy_from_slice(&1u32.to_le_bytes());
        b[458..462].copy_from_slice(&255u32.to_le_bytes());
        b[1024..1040].fill(1);
        b[1040..1056].fill(2);
        b[1056..1064].copy_from_slice(&40u64.to_le_bytes());
        b[1064..1072].copy_from_slice(&80u64.to_le_bytes());
        for (lba, other, table_lba) in [(1usize, 255u64, 2u64), (255, 1, 223)] {
            let h = &mut b[lba * 512..(lba + 1) * 512];
            h[..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&(lba as u64).to_le_bytes());
            h[32..40].copy_from_slice(&other.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes());
            h[48..56].copy_from_slice(&222u64.to_le_bytes());
            h[56..72].fill(3);
            h[72..80].copy_from_slice(&table_lba.to_le_bytes());
            h[80..84].copy_from_slice(&128u32.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
        }
        refresh_gpt(&mut b);
        b
    }
    fn refresh_gpt(b: &mut [u8]) {
        let table = b[1024..1024 + 16384].to_vec();
        b[223 * 512..255 * 512].copy_from_slice(&table);
        for lba in [1usize, 255] {
            let h = &mut b[lba * 512..(lba + 1) * 512];
            h[88..92].copy_from_slice(&crc32fast::hash(&table).to_le_bytes());
            h[16..20].fill(0);
            let crc = crc32fast::hash(&h[..92]);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
        }
    }
    fn inspect_bytes(b: &[u8]) -> Result<ImageInfo> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b)?;
        inspect(file.path())
    }
    fn add_iso(b: &mut [u8]) {
        let blocks = (b.len() / 2048) as u32;
        let p = &mut b[16 * 2048..17 * 2048];
        p[0] = 1;
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        p[80..84].copy_from_slice(&blocks.to_le_bytes());
        p[84..88].copy_from_slice(&blocks.to_be_bytes());
        p[128..130].copy_from_slice(&2048u16.to_le_bytes());
        p[130..132].copy_from_slice(&2048u16.to_be_bytes());
        let root = &mut p[156..190];
        root[0] = 34;
        root[2..6].copy_from_slice(&20u32.to_le_bytes());
        root[6..10].copy_from_slice(&20u32.to_be_bytes());
        root[10..14].copy_from_slice(&2048u32.to_le_bytes());
        root[14..18].copy_from_slice(&2048u32.to_be_bytes());
        root[25] = 2;
        root[32] = 1;
    }
    #[test]
    fn gpt_validates_primary_backup_metadata_and_partition_bounds() {
        let bytes = gpt_fixture();
        assert!(inspect_bytes(&bytes).unwrap().bootable_disk);
        for mutate in [
            |b: &mut Vec<u8>| b[255 * 512] = 0,
            |b: &mut Vec<u8>| b[223 * 512 + 48] = 42,
            |b: &mut Vec<u8>| {
                b[512 + 40..512 + 48].copy_from_slice(&2u64.to_le_bytes());
                refresh_gpt(b);
            },
            |b: &mut Vec<u8>| {
                b[255 * 512 + 72..255 * 512 + 80].copy_from_slice(&100u64.to_le_bytes());
                refresh_gpt(b);
            },
            |b: &mut Vec<u8>| {
                b[512 + 32..512 + 40].copy_from_slice(&u64::MAX.to_le_bytes());
                refresh_gpt(b);
            },
        ] {
            let mut broken = bytes.clone();
            mutate(&mut broken);
            assert!(inspect_bytes(&broken).is_err());
        }
    }
    #[test]
    fn gpt_rejects_partition_overlap_and_duplicate_identity() {
        for overlap in [false, true] {
            let mut bytes = gpt_fixture();
            let entry = bytes[1024..1152].to_vec();
            bytes[1152..1280].copy_from_slice(&entry);
            if overlap {
                bytes[1168..1184].fill(4);
                bytes[1184..1192].copy_from_slice(&79u64.to_le_bytes());
            } else {
                bytes[1184..1192].copy_from_slice(&81u64.to_le_bytes());
                bytes[1192..1200].copy_from_slice(&100u64.to_le_bytes());
            }
            refresh_gpt(&mut bytes);
            assert!(inspect_bytes(&bytes).is_err());
        }
    }
    #[test]
    fn hybrid_overlap_exception_requires_real_iso_descriptor() {
        let mut bytes = gpt_fixture();
        // Alpine/syslinux-style ISOHybrid: no protective MBR, EFI in the
        // second entry, plus a GPT ISO partition starting before FirstUsable.
        bytes[450] = 0;
        bytes[466] = 0xef;
        bytes[470..474].copy_from_slice(&40u32.to_le_bytes());
        bytes[474..478].copy_from_slice(&8u32.to_le_bytes());
        bytes[1056..1064].copy_from_slice(&0u64.to_le_bytes());
        refresh_gpt(&mut bytes);
        assert!(inspect_bytes(&bytes).is_err());
        add_iso(&mut bytes);
        assert_eq!(inspect_bytes(&bytes).unwrap().kind, ImageKind::IsoHybrid);
        bytes[16 * 2048 + 84] ^= 1;
        assert!(inspect_bytes(&bytes).is_err());
    }
    #[test]
    fn rejects_regular_mbr_overlap_but_preserves_hybrid_enclosing_partition() {
        let mut bytes = vec![0; 65536];
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        for (offset, start, len) in [(446, 0u32, 128u32), (462, 40, 8)] {
            bytes[offset + 4] = 0xef;
            bytes[offset + 8..offset + 12].copy_from_slice(&start.to_le_bytes());
            bytes[offset + 12..offset + 16].copy_from_slice(&len.to_le_bytes());
        }
        assert!(!inspect_bytes(&bytes).unwrap().bootable_disk);
        add_iso(&mut bytes);
        assert_eq!(inspect_bytes(&bytes).unwrap().kind, ImageKind::IsoHybrid);
    }
    #[test]
    fn source_open_rejects_special_files_without_blocking() {
        assert!(open_source(Path::new("/dev/null")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("source.fifo");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path is NUL terminated and points to a new owned fixture path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(open_source(&fifo).is_err());
    }
    #[test]
    fn recognizes_mbr_not_suffix() {
        let f = fixture();
        let i = inspect(f.path()).unwrap();
        assert_eq!(i.kind, ImageKind::Disk);
        assert!(i.bootable_disk);
    }
    #[test]
    fn rejects_truncated_extent() {
        let f = fixture();
        f.as_file().set_len(512).unwrap();
        assert!(!inspect(f.path()).unwrap().bootable_disk);
    }
    #[test]
    fn rejects_protective_without_gpt() {
        let mut f = fixture();
        f.seek(SeekFrom::Start(450)).unwrap();
        f.write_all(&[0xee]).unwrap();
        assert!(inspect(f.path()).is_err());
    }
    #[test]
    fn ignores_extensions_and_handles_small() {
        let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        assert!(inspect(f.path()).is_err());
    }
    #[test]
    fn rejects_source_symlink() {
        let f = fixture();
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("link");
        std::os::unix::fs::symlink(f.path(), &p).unwrap();
        assert!(inspect(&p).is_err());
    }
    #[test]
    fn malformed_inputs_never_panic() {
        let mut seed = 0x5eed_u64;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for size in [0, 1, 511, 512, 513, 32768, 34816, 65536] {
            for _ in 0..8 {
                let bytes = (0..size)
                    .map(|_| {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        (seed >> 32) as u8
                    })
                    .collect::<Vec<_>>();
                file.as_file().set_len(0).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                file.write_all(&bytes).unwrap();
                let _ = inspect(file.path());
            }
        }
    }
    #[test]
    fn lba_zero_partition_requires_optical_structure() {
        let mut file = fixture();
        file.seek(SeekFrom::Start(454)).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
        assert!(!inspect(file.path()).unwrap().bootable_disk);
    }
    #[test]
    fn cancellation_before_hash() {
        let f = fixture();
        let c = Cancellation::default();
        c.cancel();
        assert!(inspect_cancel(f.path(), &c).is_err());
    }
}
