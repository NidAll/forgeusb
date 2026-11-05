//! User-facing checksums (MD5, SHA-1, SHA-256, SHA-512) and published-sum lookup.
//! These compare a download against its publisher's list; they are not the
//! write-verification digest, which always covers the exact planned bytes.
use anyhow::{Context, Result, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, progress};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checksums {
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
    pub sha512: String,
}
impl Checksums {
    /// Case-insensitive match of a hex digest against any computed algorithm.
    pub fn matches(&self, expected: &str) -> Option<&'static str> {
        let expected = expected.trim().to_ascii_lowercase();
        [
            ("MD5", &self.md5),
            ("SHA-1", &self.sha1),
            ("SHA-256", &self.sha256),
            ("SHA-512", &self.sha512),
        ]
        .into_iter()
        .find(|(_, value)| **value == expected)
        .map(|(name, _)| name)
    }
}

/// Compute all four digests in one pass over a regular file.
pub fn compute(
    path: &Path,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<Checksums> {
    let mut file = crate::open_source(path)?;
    let size = file.metadata()?.len();
    let mut md5 = md5::Md5::new();
    let mut sha1 = sha1::Sha1::new();
    let mut sha256 = sha2::Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    loop {
        cancel.check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        md5.update(&buffer[..n]);
        sha1.update(&buffer[..n]);
        sha256.update(&buffer[..n]);
        sha512.update(&buffer[..n]);
        done += n as u64;
        emit(progress(
            Stage::Inspecting,
            done,
            size,
            "Computing checksums",
        ));
    }
    ensure!(done == size, "file changed while computing checksums");
    Ok(Checksums {
        md5: format!("{:x}", md5.finalize()),
        sha1: format!("{:x}", sha1.finalize()),
        sha256: format!("{:x}", sha256.finalize()),
        sha512: format!("{:x}", sha512.finalize()),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedSum {
    pub digest: String,
    pub list: PathBuf,
}

/// Look beside the image for a publisher checksum list naming it, such as
/// `image.iso.sha256`, `SHA256SUMS` or `sha256sum.txt`. Bounded and read-only.
pub fn find_published(image: &Path) -> Option<PublishedSum> {
    let name = image.file_name()?.to_str()?;
    let dir = image.parent()?;
    let mut candidates: Vec<PathBuf> = ["sha256", "sha256sum", "sha512", "sha1", "md5", "DIGESTS"]
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .collect();
    candidates.extend(
        [
            "SHA256SUMS",
            "SHA512SUMS",
            "SHA1SUMS",
            "MD5SUMS",
            "sha256sum.txt",
            "sha256sums.txt",
            "SHA256SUMS.txt",
            "CHECKSUM",
        ]
        .iter()
        .map(|list| dir.join(list)),
    );
    candidates.into_iter().find_map(|list| {
        read_list(&list, name)
            .ok()
            .flatten()
            .map(|digest| PublishedSum { digest, list })
    })
}

fn read_list(list: &Path, name: &str) -> Result<Option<String>> {
    let meta = fs::symlink_metadata(list)?;
    ensure!(
        meta.is_file() && meta.len() <= 1024 * 1024,
        "not a small regular file"
    );
    let text = fs::read_to_string(list).context("read checksum list")?;
    Ok(parse_list(&text, name))
}

/// Parse GNU coreutils (`HASH  name`, `HASH *name`), BSD (`SHA256 (name) = HASH`)
/// and single-digest files. Returns the digest naming `name`, or a lone digest.
pub fn parse_list(text: &str, name: &str) -> Option<String> {
    let is_hex =
        |s: &str| matches!(s.len(), 32 | 40 | 64 | 128) && s.bytes().all(|b| b.is_ascii_hexdigit());
    let mut lone = None;
    let mut lines = 0;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        lines += 1;
        if let Some((left, digest)) = line.rsplit_once(" = ")
            && let Some(file) = left
                .split_once(" (")
                .and_then(|(_, rest)| rest.strip_suffix(')'))
            && file == name
            && is_hex(digest.trim())
        {
            return Some(digest.trim().to_ascii_lowercase());
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let digest = parts.next().unwrap_or("");
        let file = parts.next().map(|f| f.trim_start().trim_start_matches('*'));
        if is_hex(digest) {
            match file {
                Some(f) if f == name || f.rsplit('/').next() == Some(name) => {
                    return Some(digest.to_ascii_lowercase());
                }
                None => lone = Some(digest.to_ascii_lowercase()),
                _ => {}
            }
        }
    }
    if lines == 1 { lone } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_common_checksum_formats() {
        let h = "a".repeat(64);
        assert_eq!(
            parse_list(
                &format!("{h}  linux.iso\n{} other.iso", "b".repeat(64)),
                "linux.iso"
            ),
            Some(h.clone())
        );
        assert_eq!(
            parse_list(&format!("{h} *linux.iso"), "linux.iso"),
            Some(h.clone())
        );
        assert_eq!(
            parse_list(&format!("SHA256 (linux.iso) = {h}"), "linux.iso"),
            Some(h.clone())
        );
        assert_eq!(
            parse_list(&h.to_ascii_uppercase(), "linux.iso"),
            Some(h.clone())
        );
        assert_eq!(parse_list(&format!("{h}  other.iso"), "linux.iso"), None);
        assert_eq!(parse_list("not a digest", "linux.iso"), None);
    }
    #[test]
    fn computes_and_matches_all_algorithms() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.img");
        fs::write(&p, b"abc").unwrap();
        let c = compute(&p, &Cancellation::default(), |_| {}).unwrap();
        assert_eq!(c.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(c.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(c.matches(&c.sha512.to_uppercase()), Some("SHA-512"));
        assert_eq!(c.matches("00"), None);
        fs::write(
            d.path().join("SHA256SUMS"),
            format!("{}  x.img\n", c.sha256),
        )
        .unwrap();
        assert_eq!(find_published(&p).unwrap().digest, c.sha256);
    }
}
