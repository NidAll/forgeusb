use crate::{
    process,
    snapshot::{self, Entry, MAX_BYTES, MAX_ENTRIES},
};
use anyhow::{Context, Result, ensure};
use forgeusb_core::Cancellation;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    path::Path,
    time::Instant,
};
#[derive(Debug)]
struct ArchiveEntry {
    path: String,
    directory: bool,
    size: u64,
}
fn listing(bytes: &[u8]) -> Result<Vec<ArchiveEntry>> {
    let text = std::str::from_utf8(bytes).context("non-UTF8 ISO names are unsupported")?;
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    let mut upper_names = BTreeSet::new();
    let mut total = 0u64;
    let mut path_bytes = 0usize;
    for block in text.split("\n\n").filter(|s| !s.trim().is_empty()) {
        let mut fields = BTreeMap::new();
        for line in block.lines() {
            let (key, value) = line.split_once(" = ").context(
                "ambiguous archive listing; control characters in paths are unsupported",
            )?;
            ensure!(
                fields.insert(key, value).is_none(),
                "ambiguous repeated archive listing field"
            );
        }
        let path = *fields.get("Path").context("ISO listing lacks path")?;
        snapshot::validate_relative(path)?;
        ensure!(
            entries.len() < MAX_ENTRIES
                && names.insert(path.to_lowercase())
                && upper_names.insert(path.to_uppercase()),
            "duplicate FAT name or excessive archive entries"
        );
        path_bytes += path.len();
        ensure!(
            path_bytes <= 16 * 1024 * 1024,
            "archive names exceed memory budget"
        );
        let directory = match fields.get("Folder") {
            Some(&"+") => true,
            Some(&"-") => false,
            _ => anyhow::bail!("ISO listing lacks file type"),
        };
        ensure!(
            fields.get("Symbolic Link").is_none_or(|v| v.is_empty())
                && fields.get("Hard Link").is_none_or(|v| v.is_empty()),
            "ISO links are unsupported"
        );
        let size = if directory {
            0
        } else {
            fields
                .get("Size")
                .context("ISO listing lacks file size")?
                .parse::<u64>()?
        };
        total = total.checked_add(size).context("archive size overflow")?;
        ensure!(
            total <= MAX_BYTES,
            "archive exceeds 128 GiB extraction safety budget"
        );
        entries.push(ArchiveEntry {
            path: path.into(),
            directory,
            size,
        });
    }
    ensure!(!entries.is_empty(), "ISO contains no files");
    // Detect a file used as a directory before creating any extraction paths.
    let files = entries
        .iter()
        .filter(|e| !e.directory)
        .map(|e| e.path.to_lowercase())
        .collect::<BTreeSet<_>>();
    for entry in &entries {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            ensure!(
                !files.contains(&p.to_string_lossy().to_lowercase()),
                "archive path has a file as parent"
            );
            parent = p.parent();
        }
    }
    Ok(entries)
}
/// Extract one explicitly selected ISO9660/UDF entry at a time to stdout and
/// write it ourselves to a validated exclusive path. The archive tool never
/// receives an output directory or permission to choose host output paths.
pub fn extract(
    source: &File,
    dest: &Path,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<(Vec<Entry>, String)> {
    let tool = process::tool("7z").or_else(|_| process::tool("7zz"))?;
    let archive = snapshot::retained_path(source);
    let mut selected = None;
    for format in ["-tudf", "-tiso"] {
        let mut args = process::args(&["l", "-slt", "-ba", "-sccUTF-8", format, "--"]);
        args.push(archive.as_os_str().to_owned());
        let mut output = Vec::new();
        if process::run(
            &tool,
            &args,
            dest,
            cancel,
            deadline,
            32 * 1024 * 1024,
            &mut output,
        )
        .is_ok()
        {
            selected = Some((format, listing(&output)?));
            break;
        }
        cancel.check()?;
    }
    let (format, entries) =
        selected.context("source is neither supported UDF nor ISO9660 installation media")?;
    let mut manifest = Vec::new();
    for entry in entries {
        cancel.check()?;
        let path = dest.join(&entry.path);
        if entry.directory {
            fs::create_dir_all(&path)?;
            continue;
        }
        fs::create_dir_all(path.parent().context("archive path lacks parent")?)?;
        let mut output = snapshot::exclusive(&path)?;
        let mut args =
            process::args(&["x", "-so", "-y", "-spd", "-ssc", "-sccUTF-8", format, "--"]);
        args.push(archive.as_os_str().to_owned());
        args.push(OsString::from(&entry.path));
        let bytes = process::run(
            &tool,
            &args,
            dest,
            cancel,
            deadline,
            entry.size,
            &mut output,
        )?;
        ensure!(
            bytes == entry.size,
            "extracted file differs from declared size"
        );
        let sha256 = snapshot::hash(&mut output, cancel, deadline)?;
        manifest.push(Entry {
            relative: entry.path.into(),
            bytes,
            sha256,
        });
    }
    manifest.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok((
        manifest,
        if format == "-tudf" {
            "udf_iso"
        } else {
            "iso9660"
        }
        .into(),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_archive_traversal_collision_and_links() {
        for text in [
            "Path = ../escape\nFolder = -\nSize = 1\n\n",
            "Path = A\nFolder = -\nSize = 1\n\nPath = a\nFolder = -\nSize = 1\n\n",
            "Path = a\nFolder = -\nSize = 1\nSymbolic Link = /etc/passwd\n\n",
            "Path = x\nFolder = -\nSize = 1\n\nPath = x/y\nFolder = -\nSize = 1\n\n",
        ] {
            assert!(listing(text.as_bytes()).is_err());
        }
    }
}
