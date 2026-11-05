//! Data-range coverage for ForgeUSB-prepared, device-sized sparse images.
//!
//! A prepared image may be as large as the target device while holding only a
//! few GiB of filesystem data. Its unallocated regions are "don't care" free
//! space, so writing them would only cost time. Ordinary user images never use
//! this mode: their holes are part of the content and are written as zeros.
//!
//! The write set is every data extent the filesystem reports, merged across
//! small gaps, plus the first and last [`EDGE_BYTES`] of the image. The edges
//! are always written (zeros where the image has holes) so stale partition
//! tables or filesystem signatures from earlier media cannot survive there.
use anyhow::{Context, Result, ensure};
use forgeusb_core::Cancellation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::io::AsRawFd,
};

/// Always-written region at both ends of a sparse image.
pub const EDGE_BYTES: u64 = 2 * 1024 * 1024;
/// Holes shorter than this are folded into the surrounding data range.
pub const MERGE_GAP: u64 = 1024 * 1024;
/// Upper bound on serialized ranges; keeps plans well below the 4 MiB limit.
pub const MAX_RANGES: usize = 16_384;
const ALIGN: u64 = 4096;
const DOMAIN: &[u8] = b"forgeusb-data-ranges-v1\0";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Every byte of the image is content. Digest is plain SHA-256.
    #[default]
    Full,
    /// Only [`ImageInfo::ranges`](crate::ImageInfo) are content. Digest binds the ranges.
    DataRanges,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}
impl ByteRange {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

pub fn total(ranges: &[ByteRange]) -> u64 {
    ranges.iter().map(ByteRange::len).sum()
}

/// Ordered, non-overlapping ranges that must be written for a prepared image.
pub fn write_ranges(file: &File, size: u64) -> Result<Vec<ByteRange>> {
    let mut data = data_extents(file, size)?;
    data.push(ByteRange {
        start: 0,
        end: size.min(EDGE_BYTES),
    });
    data.push(ByteRange {
        start: size.saturating_sub(EDGE_BYTES),
        end: size,
    });
    normalize(data, size)
}

/// Sort, align outward to 4 KiB, clamp, and merge across gaps under [`MERGE_GAP`].
pub fn normalize(mut ranges: Vec<ByteRange>, size: u64) -> Result<Vec<ByteRange>> {
    for r in &mut ranges {
        r.start = r.start / ALIGN * ALIGN;
        r.end = r.end.div_ceil(ALIGN).saturating_mul(ALIGN).min(size);
    }
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<ByteRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end.saturating_add(MERGE_GAP) => {
                last.end = last.end.max(r.end);
            }
            _ => merged.push(r),
        }
    }
    ensure!(
        merged.len() <= MAX_RANGES,
        "image is too fragmented ({} data ranges); copy it to a new file and retry",
        merged.len()
    );
    Ok(merged)
}

/// Validate a serialized range list before trusting it for I/O.
pub fn validate(ranges: &[ByteRange], size: u64) -> Result<()> {
    ensure!(
        !ranges.is_empty() && ranges.len() <= MAX_RANGES,
        "invalid data-range count"
    );
    let mut previous = 0u64;
    for (i, r) in ranges.iter().enumerate() {
        ensure!(
            r.start < r.end && r.end <= size && (i == 0 || r.start > previous),
            "data ranges must be ordered, non-empty, disjoint and inside the image"
        );
        previous = r.end;
    }
    Ok(())
}

fn data_extents(file: &File, size: u64) -> Result<Vec<ByteRange>> {
    let fd = file.as_raw_fd();
    let mut extents = Vec::new();
    let mut at = 0u64;
    while at < size {
        let start = unsafe { libc::lseek(fd, at as libc::off_t, libc::SEEK_DATA) };
        if start < 0 {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ENXIO) => break,
                // Filesystems without hole reporting: every byte is data.
                Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) if extents.is_empty() => {
                    return Ok(vec![ByteRange {
                        start: 0,
                        end: size,
                    }]);
                }
                _ => return Err(error).context("query image data extents"),
            }
        }
        let start = start as u64;
        if start >= size {
            break;
        }
        let end = unsafe { libc::lseek(fd, start as libc::off_t, libc::SEEK_HOLE) };
        ensure!(end >= 0, "query image hole extents failed");
        let end = (end as u64).min(size);
        ensure!(end > start, "filesystem reported an empty data extent");
        extents.push(ByteRange { start, end });
        ensure!(
            extents.len() <= MAX_RANGES * 64,
            "image is too fragmented to map safely"
        );
        at = end;
    }
    Ok(extents)
}

/// Incremental digest shared by inspection, writing and verification.
pub struct Digester {
    coverage: Coverage,
    hash: Sha256,
}
impl Digester {
    pub fn new(coverage: Coverage, size: u64) -> Self {
        let mut hash = Sha256::new();
        if coverage == Coverage::DataRanges {
            hash.update(DOMAIN);
            hash.update(size.to_le_bytes());
        }
        Self { coverage, hash }
    }
    /// Must be called before the bytes of each range in [`Coverage::DataRanges`].
    pub fn begin(&mut self, range: ByteRange) {
        if self.coverage == Coverage::DataRanges {
            self.hash.update(range.start.to_le_bytes());
            self.hash.update(range.end.to_le_bytes());
        }
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.hash.update(bytes);
    }
    pub fn finish(self) -> String {
        format!("{:x}", self.hash.finalize())
    }
}

/// Ranges to read for a coverage mode: the full image or the recorded data ranges.
pub fn effective(coverage: Coverage, ranges: &[ByteRange], size: u64) -> Vec<ByteRange> {
    match coverage {
        Coverage::Full => vec![ByteRange {
            start: 0,
            end: size,
        }],
        Coverage::DataRanges => ranges.to_vec(),
    }
}

/// Digest an open file over the given coverage, reading only the listed ranges.
pub fn digest(
    file: &mut File,
    coverage: Coverage,
    ranges: &[ByteRange],
    size: u64,
    cancel: &Cancellation,
) -> Result<String> {
    let mut digester = Digester::new(coverage, size);
    let mut buffer = vec![0u8; 1024 * 1024];
    for range in effective(coverage, ranges, size) {
        digester.begin(range);
        file.seek(SeekFrom::Start(range.start))?;
        let mut left = range.len();
        while left > 0 {
            cancel.check()?;
            let n = left.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..n])
                .context("image shrank while hashing")?;
            digester.update(&buffer[..n]);
            left -= n as u64;
        }
    }
    Ok(digester.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sparse_file_maps_data_and_edges_only() {
        let mut f = tempfile::tempfile().unwrap();
        let size = 64 * 1024 * 1024;
        f.set_len(size).unwrap();
        f.seek(SeekFrom::Start(20 * 1024 * 1024)).unwrap();
        f.write_all(&[7u8; 8192]).unwrap();
        f.sync_all().unwrap();
        let ranges = write_ranges(&f, size).unwrap();
        validate(&ranges, size).unwrap();
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, size);
        assert!(
            ranges
                .iter()
                .any(|r| r.start <= 20 * 1024 * 1024 && r.end >= 20 * 1024 * 1024 + 8192)
        );
        // On filesystems with hole reporting, most of the image is skipped.
        assert!(total(&ranges) <= size);
    }

    #[test]
    fn normalize_merges_small_gaps_and_rejects_bad_lists() {
        let r = normalize(
            vec![
                ByteRange { start: 0, end: 10 },
                ByteRange {
                    start: 100,
                    end: 200,
                },
                ByteRange {
                    start: 10 * MERGE_GAP,
                    end: 10 * MERGE_GAP + 1,
                },
            ],
            100 * MERGE_GAP,
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0],
            ByteRange {
                start: 0,
                end: 4096
            }
        );
        assert!(validate(&[ByteRange { start: 5, end: 5 }], 10).is_err());
        assert!(
            validate(
                &[
                    ByteRange { start: 0, end: 8 },
                    ByteRange { start: 4, end: 9 }
                ],
                10
            )
            .is_err()
        );
        assert!(validate(&[ByteRange { start: 0, end: 11 }], 10).is_err());
    }

    #[test]
    fn data_range_digest_binds_ranges() {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&[1u8; 8192]).unwrap();
        let c = Cancellation::default();
        let a = digest(
            &mut f,
            Coverage::DataRanges,
            &[ByteRange {
                start: 0,
                end: 4096,
            }],
            8192,
            &c,
        )
        .unwrap();
        let b = digest(
            &mut f,
            Coverage::DataRanges,
            &[ByteRange {
                start: 4096,
                end: 8192,
            }],
            8192,
            &c,
        )
        .unwrap();
        assert_ne!(a, b, "identical bytes at different offsets must differ");
        let full = digest(&mut f, Coverage::Full, &[], 8192, &c).unwrap();
        assert_eq!(full, format!("{:x}", Sha256::digest([1u8; 8192])));
    }
}
