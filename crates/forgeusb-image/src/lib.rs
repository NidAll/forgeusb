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
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open source {} (symlinks rejected)", path.display()))
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
    f.seek(SeekFrom::Start(0))?;
    let stamp = FileStamp::of(f)?;
    ensure!(stamp.size >= 512, "image is smaller than one disk sector");
    let mut head = [0u8; 512];
    f.read_exact(&mut head)?;
    let compressed = head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0])
        || head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || head.starts_with(b"BZh");
    let mut disk = valid_disk(f, &head, stamp.size)?;
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
                windows = iso_has_windows(f, &d[156..190], stamp.size)?;
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
fn valid_disk(f: &mut File, h: &[u8; 512], size: u64) -> Result<bool> {
    if h[510..512] != [0x55, 0xaa] {
        return Ok(false);
    }
    let mut entries = 0;
    let mut protective = false;
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
            protective = true;
            continue;
        }
        if start
            .checked_add(len)
            .and_then(|n| n.checked_mul(512))
            .is_none_or(|end| end > size)
        {
            return Ok(false);
        }
    }
    if protective {
        validate_gpt(f, size)?;
    }
    Ok(entries > 0)
}
fn validate_gpt(f: &mut File, size: u64) -> Result<()> {
    f.seek(SeekFrom::Start(512))?;
    let mut h = [0u8; 512];
    f.read_exact(&mut h)?;
    ensure!(&h[..8] == b"EFI PART", "protective MBR without GPT header");
    let len = le32(&h[12..16]) as usize;
    ensure!((92..=512).contains(&len), "invalid GPT header size");
    let crc = le32(&h[16..20]);
    h[16..20].fill(0);
    ensure!(crc32fast::hash(&h[..len]) == crc, "GPT header CRC mismatch");
    let u64_at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().expect("GPT fixed slice"));
    ensure!(
        u64_at(24) == 1
            && u64_at(32) < size / 512
            && u64_at(40) <= u64_at(48)
            && u64_at(48) < size / 512,
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
    let off = u64_at(72).checked_mul(512).context("GPT offset overflow")?;
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
    for e in table.chunks_exact(entry as usize) {
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64::from_le_bytes(e[32..40].try_into()?);
        let last = u64::from_le_bytes(e[40..48].try_into()?);
        ensure!(
            first >= u64_at(40) && first <= last && last <= u64_at(48),
            "GPT partition outside usable range"
        );
    }
    Ok(())
}
fn iso_has_windows(f: &mut File, root: &[u8], size: u64) -> Result<bool> {
    if root.len() < 34 || root[0] < 34 {
        bail!("invalid ISO root directory");
    }
    let offset = (le32(&root[2..6]) as u64) * 2048;
    let len = le32(&root[10..14]) as u64;
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
            n >= 34 && at + n <= bytes.len(),
            "malformed ISO directory record"
        );
        let r = &bytes[at..at + n];
        let namesize = r[32] as usize;
        ensure!(33 + namesize <= r.len(), "malformed ISO filename");
        let name = String::from_utf8_lossy(&r[33..33 + namesize]).to_ascii_uppercase();
        sources |= name == "SOURCES";
        bootmgr |= name.starts_with("BOOTMGR");
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
