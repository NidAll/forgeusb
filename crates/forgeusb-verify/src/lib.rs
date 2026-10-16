//! Complete byte comparison of the source-sized prefix, with bounded buffers.
use anyhow::{Result, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, progress};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Verification {
    pub bytes: u64,
    pub sha256: String,
}
pub fn compare<S: Read + Seek, T: Read + Seek>(
    source: &mut S,
    target: &mut T,
    size: u64,
    expected_hash: &str,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<Verification> {
    source.seek(SeekFrom::Start(0))?;
    target.seek(SeekFrom::Start(0))?;
    let mut a = vec![0u8; 1024 * 1024];
    let mut b = vec![0u8; 1024 * 1024];
    let mut at = 0u64;
    let mut hash = Sha256::new();
    while at < size {
        cancel.check()?;
        let n = ((size - at).min(a.len() as u64)) as usize;
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
        emit(progress(
            Stage::Verifying,
            at,
            size,
            "Comparing every source byte",
        ));
    }
    let sha256 = format!("{:x}", hash.finalize());
    ensure!(
        sha256 == expected_hash,
        "source/read-back checksum differs from approved plan"
    );
    Ok(Verification { bytes: at, sha256 })
}
#[cfg(test)]
mod tests {
    use super::*;
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
}
