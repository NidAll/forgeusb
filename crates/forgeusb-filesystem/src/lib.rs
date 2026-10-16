//! Filesystem compatibility policy; no formatter is invoked here.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const FAT32_MAX_FILE_BYTES: u64 = u32::MAX as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    Fat32,
    Ntfs,
    Exfat,
    Ext4,
}

pub fn validate_file_size(filesystem: Filesystem, bytes: u64) -> Result<()> {
    ensure!(
        filesystem != Filesystem::Fat32 || bytes <= FAT32_MAX_FILE_BYTES,
        "file is {bytes} bytes; FAT32 allows at most {FAT32_MAX_FILE_BYTES}; split install.wim with wimlib or use a separately validated NTFS boot strategy"
    );
    Ok(())
}

/// Conservative FAT/Windows name policy for future extraction, not a full LFN codec.
pub fn validate_fat_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name != "." && name != "..",
        "empty or traversal component"
    );
    ensure!(
        name.encode_utf16().count() <= 255,
        "FAT filename exceeds 255 UTF-16 code units"
    );
    ensure!(
        !name.ends_with([' ', '.']),
        "FAT/Windows name ends with space or period"
    );
    ensure!(
        !name.chars().any(|c| c < ' ' || "\\/:*?\"<>|".contains(c)),
        "forbidden FAT/Windows filename character"
    );
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    ensure!(
        !reserved.contains(&stem.as_str()),
        "reserved Windows filename"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fat_size_boundary() {
        assert!(validate_file_size(Filesystem::Fat32, FAT32_MAX_FILE_BYTES).is_ok());
        assert!(validate_file_size(Filesystem::Fat32, FAT32_MAX_FILE_BYTES + 1).is_err());
        assert!(validate_file_size(Filesystem::Ntfs, FAT32_MAX_FILE_BYTES + 1).is_ok());
    }
    #[test]
    fn rejects_ambiguous_names() {
        for name in ["..", "CON.txt", "install.wim.", "a/b", "a:b", "a\\b", "aux"] {
            assert!(validate_fat_name(name).is_err(), "{name}");
        }
        assert!(validate_fat_name("install2.swm").is_ok());
        assert!(validate_fat_name(&"😀".repeat(128)).is_err());
    }
}
