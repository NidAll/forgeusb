//! Complete byte comparison of the source-sized prefix, with bounded buffers.
use anyhow::{Result, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, progress};
use forgeusb_image::{ByteRange, Coverage, sparse};
use std::io::{Read, Seek, SeekFrom};
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Verification {
    /// Logical image size covered by the verification.
    pub bytes: u64,
    pub sha256: String,
    /// Bytes actually read back and compared.
    #[serde(default)]
    pub compared: u64,
}
pub fn compare<S: Read + Seek, T: Read + Seek>(
    source: &mut S,
    target: &mut T,
    size: u64,
    expected_hash: &str,
    cancel: &Cancellation,
    emit: impl FnMut(Progress),
) -> Result<Verification> {
    compare_ranges(
        source,
        target,
        Coverage::Full,
        &[],
        size,
        expected_hash,
        cancel,
        emit,
    )
}
/// Compare the planned coverage byte for byte and recompute its digest from target reads.
#[allow(clippy::too_many_arguments)]
pub fn compare_ranges<S: Read + Seek, T: Read + Seek>(
    source: &mut S,
    target: &mut T,
    coverage: Coverage,
    ranges: &[ByteRange],
    size: u64,
    expected_hash: &str,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<Verification> {
    let ranges = sparse::effective(coverage, ranges, size);
    if coverage == Coverage::DataRanges {
        sparse::validate(&ranges, size)?;
    }
    let total = sparse::total(&ranges);
    let mut a = vec![0u8; 1024 * 1024];
    let mut b = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    let mut hash = sparse::Digester::new(coverage, size);
    for range in ranges {
        hash.begin(range);
        source.seek(SeekFrom::Start(range.start))?;
        target.seek(SeekFrom::Start(range.start))?;
        let mut at = range.start;
        while at < range.end {
            cancel.check()?;
            let n = ((range.end - at).min(a.len() as u64)) as usize;
            source.read_exact(&mut a[..n])?;
            target.read_exact(&mut b[..n])?;
            if a[..n] != b[..n] {
                let mismatch = a[..n]
                    .iter()
                    .zip(&b[..n])
                    .position(|(x, y)| x != y)
                    .unwrap_or(0);
                anyhow::bail!("read-back mismatch at byte {}", at + mismatch as u64);
            }
            hash.update(&b[..n]);
            at += n as u64;
            done += n as u64;
            emit(progress(
                Stage::Verifying,
                done,
                total,
                "Comparing every written byte",
            ));
        }
    }
    let sha256 = hash.finish();
    ensure!(
        sha256 == expected_hash,
        "source/read-back checksum differs from approved plan"
    );
    Ok(Verification {
        bytes: size,
        sha256,
        compared: done,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;
    #[test]
    fn detects_mismatch_and_short_reads() {
        let data = b"abc";
        let hash = format!("{:x}", Sha256::digest(data));
        assert!(
            compare(
                &mut Cursor::new(data),
                &mut Cursor::new(b"abcTAIL"),
                3,
                &hash,
                &Cancellation::default(),
                |_| {}
            )
            .is_ok()
        );
        assert!(
            compare(
                &mut Cursor::new(data),
                &mut Cursor::new(b"abd"),
                3,
                &hash,
                &Cancellation::default(),
                |_| {}
            )
            .is_err()
        );
        assert!(
            compare(
                &mut Cursor::new(data),
                &mut Cursor::new(b"a"),
                3,
                &hash,
                &Cancellation::default(),
                |_| {}
            )
            .is_err()
        );
    }
    #[test]
    fn data_ranges_ignore_holes_but_bind_ranges() {
        let source = [1u8, 1, 0, 0, 2, 2];
        let stale = [1u8, 1, 9, 9, 2, 2];
        let ranges = [
            ByteRange { start: 0, end: 2 },
            ByteRange { start: 4, end: 6 },
        ];
        let mut f = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut f, &source).unwrap();
        let expected = sparse::digest(
            &mut f,
            Coverage::DataRanges,
            &ranges,
            6,
            &Cancellation::default(),
        )
        .unwrap();
        let v = compare_ranges(
            &mut Cursor::new(source),
            &mut Cursor::new(stale),
            Coverage::DataRanges,
            &ranges,
            6,
            &expected,
            &Cancellation::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!((v.bytes, v.compared), (6, 4));
        assert!(
            compare_ranges(
                &mut Cursor::new(source),
                &mut Cursor::new(stale),
                Coverage::Full,
                &[],
                6,
                &expected,
                &Cancellation::default(),
                |_| {},
            )
            .is_err()
        );
    }
}
