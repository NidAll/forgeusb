//! Capability decisions do not install a bootloader or infer a boot-test result.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Firmware {
    PreserveImage,
    Uefi,
    Bios,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageCapabilities {
    pub disk_layout: bool,
    pub iso9660: bool,
    pub windows_installer: bool,
    pub efi_fallback: bool,
    pub oversized_install_wim: bool,
    pub compressed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Strategy {
    pub id: String,
    pub executable: bool,
    pub explanation: String,
    pub verification: String,
}

pub fn select_strategy(image: &ImageCapabilities, firmware: Firmware) -> Result<Strategy> {
    ensure!(
        !image.compressed,
        "compressed streaming is not implemented; decompress into a regular image file with a trusted tool and inspect it again"
    );
    if image.windows_installer {
        ensure!(
            firmware != Firmware::Bios,
            "Windows legacy BIOS installation is not implemented"
        );
        ensure!(
            image.efi_fallback,
            "Windows UEFI preparation requires a validated EFI fallback loader for the target architecture"
        );
        return Ok(Strategy {
            id: if image.oversized_install_wim { "windows_fat32_split_wim" } else { "windows_fat32" }.into(),
            executable: false,
            explanation: "Windows ISO media requires extraction and filesystem installation; only tree inspection and standalone WIM staging are currently available. No raw-write fallback is permitted.".into(),
            verification: "Future executor must compare copied files, check split WIMs and architecture-specific EFI paths, then boot-test. No boot result is implied.".into(),
        });
    }
    if image.disk_layout {
        ensure!(
            firmware == Firmware::PreserveImage,
            "raw writing preserves the image partition table and firmware support; choose preserve_image rather than overriding firmware"
        );
        return Ok(Strategy {
            id: "raw_image".into(), executable: true,
            explanation: "Recognized disk layout: preserve the source bytes, partition table, filesystems and bootloaders.".into(),
            verification: "Compare every written byte with the source; bytes beyond the source length and actual firmware bootability are outside this check.".into(),
        });
    }
    if image.iso9660 {
        bail!(
            "ISO lacks a recognized disk layout; generic extraction can break bootloader configuration. Use a vendor USB disk image or wait for a validated distribution-specific extraction strategy"
        );
    }
    bail!("no supported boot strategy; inspect a structurally recognized disk image")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_does_not_override_firmware() {
        let image = ImageCapabilities {
            disk_layout: true,
            ..Default::default()
        };
        assert!(
            select_strategy(&image, Firmware::PreserveImage)
                .unwrap()
                .executable
        );
        assert!(select_strategy(&image, Firmware::Uefi).is_err());
    }
    #[test]
    fn windows_never_falls_back_to_raw() {
        let image = ImageCapabilities {
            disk_layout: true,
            windows_installer: true,
            efi_fallback: true,
            oversized_install_wim: true,
            ..Default::default()
        };
        let s = select_strategy(&image, Firmware::Uefi).unwrap();
        assert_eq!(s.id, "windows_fat32_split_wim");
        assert!(!s.executable);
    }
    #[test]
    fn unsupported_rejected() {
        assert!(select_strategy(&ImageCapabilities::default(), Firmware::PreserveImage).is_err());
        assert!(
            select_strategy(
                &ImageCapabilities {
                    iso9660: true,
                    ..Default::default()
                },
                Firmware::PreserveImage
            )
            .is_err()
        );
        assert!(
            select_strategy(
                &ImageCapabilities {
                    disk_layout: true,
                    compressed: true,
                    ..Default::default()
                },
                Firmware::PreserveImage
            )
            .is_err()
        );
    }
}
