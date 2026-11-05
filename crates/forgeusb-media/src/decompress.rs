//! Unprivileged staging of compressed disk images (`.gz .xz .zst .bz2 .zip`).
//!
//! Decoding runs in the desktop user's process, never in the root helper. The
//! expanded image is written as a new sparse regular file (runs of zeros become
//! holes, which read back as zeros), published without clobbering, then planned
//! and written like any ordinary image with full coverage.
use crate::snapshot;
use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, progress};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    Gzip,
    Xz,
    Zstd,
    Bzip2,
    Zip,
}
impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Xz => "xz",
            Self::Zstd => "zstd",
            Self::Bzip2 => "bzip2",
            Self::Zip => "zip",
        }
    }
}

/// Identify a compressed image by its leading bytes, never by its suffix.
pub fn detect_bytes(head: &[u8]) -> Option<Compression> {
    if head.starts_with(&[0x1f, 0x8b]) {
        Some(Compression::Gzip)
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Compression::Xz)
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Compression::Zstd)
    } else if head.starts_with(b"BZh") && head.get(3).is_some_and(|d| (b'1'..=b'9').contains(d)) {
        Some(Compression::Bzip2)
    } else if head.starts_with(b"PK\x03\x04") {
        Some(Compression::Zip)
    } else {
        None
    }
}
pub fn detect(path: &Path) -> Result<Option<Compression>> {
    let mut f = snapshot::open_regular(path)?;
    let mut head = [0u8; 8];
    let n = f.read(&mut head)?;
    Ok(detect_bytes(&head[..n]))
}

/// Suggested name for the expanded image: `image.img.xz` → `image.img`.
pub fn expanded_name(source: &Path) -> String {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image.img");
    for suffix in [".gz", ".xz", ".zst", ".zstd", ".bz2", ".zip"] {
        if let Some(stem) = name.strip_suffix(suffix)
            && !stem.is_empty()
        {
            return if stem.contains('.') {
                stem.to_owned()
            } else {
                format!("{stem}.img")
            };
        }
    }
    format!("{name}.img")
}

#[derive(Debug, Clone, Serialize)]
pub struct DecompressReport {
    pub output: PathBuf,
    pub compression: Compression,
    /// SHA-256 of the compressed file, for comparison with the publisher's checksum.
    pub source_sha256: String,
    pub source_bytes: u64,
    /// Expanded image size.
    pub bytes: u64,
    /// Expanded bytes that were nonzero and stored (the rest are holes).
    pub stored_bytes: u64,
}

/// Counts and hashes compressed input as decoders consume it.
struct Tee<R> {
    inner: R,
    hash: Sha256,
    read: Arc<AtomicU64>,
}
impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

struct MultiZstd<R: BufRead> {
    source: Option<R>,
    frame: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
}
impl<R: BufRead> Read for MultiZstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(frame) = &mut self.frame {
                let n = frame.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                let decoder = self.frame.take().expect("frame present");
                self.source = Some(decoder.into_inner());
            }
            let mut source = self.source.take().expect("source present");
            if source.fill_buf()?.is_empty() {
                self.source = Some(source);
                return Ok(0);
            }
            let frame = ruzstd::decoding::StreamingDecoder::new(source)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            self.frame = Some(frame);
        }
    }
}

/// Expand `source` into the new file `output` (which must not exist).
/// `limit` bounds the expanded size, e.g. the selected target's capacity.
pub fn decompress_image(
    source: &Path,
    output: &Path,
    limit: u64,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<DecompressReport> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "decompression must run as an ordinary user"
    );
    ensure!(limit >= 512, "expanded-size limit is too small");
    ensure!(
        fs::symlink_metadata(output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "output already exists or is inaccessible"
    );
    let input = snapshot::open_regular(source)?;
    let before = input.metadata()?;
    let source_bytes = before.len();
    let mut head = [0u8; 8];
    let got = (&input).take(8).read(&mut head)?;
    let compression =
        detect_bytes(&head[..got]).context("source is not a recognized compressed image")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let name = output.file_name().context("output requires a filename")?;
    let mut staged = tempfile::Builder::new()
        .prefix(".forgeusb-expand-")
        .tempfile_in(&parent)?;
    let consumed = Arc::new(AtomicU64::new(0));
    let mut file = input.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let (source_sha256, bytes, stored) = match compression {
        Compression::Zip => {
            // The archive is hashed in a separate pass: zip reading seeks.
            let sha = snapshot::hash(
                &mut file.try_clone()?,
                cancel,
                std::time::Instant::now() + std::time::Duration::from_secs(7200),
            )?;
            file.seek(SeekFrom::Start(0))?;
            let mut archive =
                zip::ZipArchive::new(BufReader::new(file)).context("read zip archive")?;
            let mut chosen = None;
            for i in 0..archive.len() {
                let entry = archive.by_index_raw(i)?;
                if entry.is_dir() {
                    continue;
                }
                ensure!(
                    chosen.is_none(),
                    "zip archive holds several files; extract the disk image you want first"
                );
                ensure!(
                    !entry.encrypted(),
                    "encrypted zip archives are not supported"
                );
                ensure!(entry.is_file(), "zip entry is not a regular file");
                chosen = Some((i, entry.size()));
            }
            let (index, size) = chosen.context("zip archive holds no file")?;
            ensure!(
                size <= limit,
                "zip entry expands to {size} bytes, more than the {limit}-byte limit"
            );
            let entry = archive.by_index(index)?;
            let (bytes, stored) = expand(
                entry,
                staged.as_file_mut(),
                limit,
                size,
                cancel,
                &mut emit,
                None,
            )?;
            ensure!(
                bytes == size,
                "zip entry size does not match its directory record"
            );
            (sha, bytes, stored)
        }
        _ => {
            let tee = Tee {
                inner: BufReader::with_capacity(1024 * 1024, file),
                hash: Sha256::new(),
                read: consumed.clone(),
            };
            let mut tee = tee;
            let (bytes, stored) = {
                let reader: Box<dyn Read + '_> = match compression {
                    Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(&mut tee)),
                    Compression::Xz => Box::new(lzma_rust2::XzReader::new(&mut tee, true)),
                    Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(&mut tee)),
                    Compression::Zstd => Box::new(MultiZstd {
                        source: Some(BufReader::new(&mut tee)),
                        frame: None,
                    }),
                    Compression::Zip => unreachable!(),
                };
                expand(
                    reader,
                    staged.as_file_mut(),
                    limit,
                    source_bytes,
                    cancel,
                    &mut emit,
                    Some(&consumed),
                )?
            };
            // Drain anything the decoder left (it must be nothing but padding).
            let mut rest = Vec::new();
            (&mut tee).take(1024 * 1024).read_to_end(&mut rest)?;
            ensure!(
                rest.iter().all(|&b| b == 0),
                "unexpected data after the end of the compressed stream"
            );
            ensure!(
                consumed.load(Ordering::Relaxed) == source_bytes,
                "compressed stream ended before the end of the file"
            );
            (format!("{:x}", tee.hash.finalize()), bytes, stored)
        }
    };
    snapshot::stable(&before, &input.metadata()?)?;
    ensure!(
        bytes >= 512,
        "expanded image is smaller than one disk sector"
    );
    staged.as_file_mut().set_len(bytes)?;
    emit(progress(
        Stage::Flushing,
        bytes,
        bytes,
        "Flushing expanded image",
    ));
    staged.as_file_mut().sync_all()?;
    cancel.check()?;
    let output = parent.join(name);
    staged
        .persist_noclobber(&output)
        .map_err(|e| e.error)
        .context("publishing expanded image without overwriting existing files")?;
    File::open(&parent)?.sync_all()?;
    emit(progress(
        Stage::Complete,
        bytes,
        bytes,
        "Expanded image ready",
    ));
    Ok(DecompressReport {
        output,
        compression,
        source_sha256,
        source_bytes,
        bytes,
        stored_bytes: stored,
    })
}

fn expand(
    mut reader: impl Read,
    out: &mut File,
    limit: u64,
    progress_total: u64,
    cancel: &Cancellation,
    emit: &mut impl FnMut(Progress),
    consumed: Option<&AtomicU64>,
) -> Result<(u64, u64)> {
    const CHUNK: usize = 1024 * 1024;
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    let mut stored = 0u64;
    loop {
        cancel.check()?;
        let mut filled = 0;
        while filled < CHUNK {
            let n = match reader.read(&mut buffer[filled..]) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => bail!("corrupt or truncated compressed image: {e}"),
            };
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        total = total.checked_add(filled as u64).context("size overflow")?;
        ensure!(
            total <= limit,
            "expanded image exceeds the {limit}-byte limit (target capacity); refusing to continue"
        );
        if buffer[..filled].iter().all(|&b| b == 0) {
            out.seek(SeekFrom::Current(filled as i64))?;
        } else {
            out.write_all(&buffer[..filled])
                .context("write expanded image (is the disk full?)")?;
            stored += filled as u64;
        }
        let done = consumed.map_or(total, |c| c.load(Ordering::Relaxed));
        emit(progress(
            Stage::Inspecting,
            done.min(progress_total),
            progress_total,
            "Decompressing image",
        ));
        if filled < CHUNK {
            break;
        }
    }
    Ok((total, stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Vec<u8> {
        let mut data = vec![0u8; 3 * 1024 * 1024 + 4096];
        data[..512].copy_from_slice(&[0x5a; 512]);
        data[510] = 0x55;
        data[511] = 0xaa;
        let tail = data.len() - 100;
        data[tail..].fill(7);
        data
    }

    fn roundtrip(compressed: &[u8], kind: Compression) {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("image.img.c");
        fs::write(&src, compressed).unwrap();
        assert_eq!(detect(&src).unwrap(), Some(kind));
        let out = d.path().join("image.img");
        let r = decompress_image(&src, &out, 64 << 20, &Cancellation::default(), |_| {}).unwrap();
        assert_eq!(fs::read(&out).unwrap(), payload());
        assert_eq!(r.bytes, payload().len() as u64);
        assert!(r.stored_bytes < r.bytes, "zero runs should stay sparse");
        assert_eq!(r.source_sha256, format!("{:x}", Sha256::digest(compressed)));
        // Never overwrites, and the limit is enforced.
        assert!(decompress_image(&src, &out, 64 << 20, &Cancellation::default(), |_| {}).is_err());
        let small = d.path().join("small.img");
        assert!(decompress_image(&src, &small, 1 << 20, &Cancellation::default(), |_| {}).is_err());
        assert!(!small.exists());
    }

    #[test]
    fn gzip_and_bzip2_roundtrip() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&payload()).unwrap();
        roundtrip(&gz.finish().unwrap(), Compression::Gzip);
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bz.write_all(&payload()).unwrap();
        roundtrip(&bz.finish().unwrap(), Compression::Bzip2);
    }

    #[test]
    fn xz_zstd_and_zip_roundtrip_with_system_tools() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("raw.img");
        fs::write(&raw, payload()).unwrap();
        for (tool, args, kind) in [
            ("xz", vec!["-k", "-T1", "-c"], Compression::Xz),
            ("zstd", vec!["-q", "-c"], Compression::Zstd),
        ] {
            let Ok(path) = crate::process::tool(tool) else {
                continue;
            };
            let out = std::process::Command::new(path)
                .args(&args)
                .arg(&raw)
                .output()
                .unwrap();
            assert!(out.status.success());
            roundtrip(&out.stdout, kind);
        }
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("disk.img", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&payload()).unwrap();
        roundtrip(&zip.finish().unwrap().into_inner(), Compression::Zip);
    }

    #[test]
    fn corrupt_input_never_publishes() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&payload()).unwrap();
        let mut bytes = gz.finish().unwrap();
        let mid = bytes.len() / 2;
        bytes.truncate(mid);
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("broken.img.gz");
        fs::write(&src, &bytes).unwrap();
        let out = d.path().join("broken.img");
        assert!(decompress_image(&src, &out, 64 << 20, &Cancellation::default(), |_| {}).is_err());
        assert!(!out.exists());
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn names_expanded_output() {
        assert_eq!(expanded_name(Path::new("/a/pi.img.xz")), "pi.img");
        assert_eq!(expanded_name(Path::new("/a/disk.zip")), "disk.img");
        assert_eq!(expanded_name(Path::new("/a/odd")), "odd.img");
    }
}
