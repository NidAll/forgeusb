//! Windows Setup answer file (`autounattend.xml`) for common installer tweaks.
//!
//! The file is generated from fixed templates and placed at the media root,
//! where Windows Setup discovers it. No Microsoft file is modified. Passwords are
//! never handled: a requested local account starts with an empty password that
//! Windows asks the user to change at first sign-in.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Customization {
    /// Skip Windows 11 TPM 2.0, Secure Boot and 4 GB RAM setup checks.
    #[serde(default)]
    pub bypass_requirements: bool,
    /// Allow finishing setup without a Microsoft account or network.
    #[serde(default)]
    pub bypass_online_account: bool,
    /// Create this local administrator account (empty password, change at first sign-in).
    #[serde(default)]
    pub local_account: Option<String>,
    /// BCP-47 locale such as `fr-FR` for input, system and user locale.
    #[serde(default)]
    pub regional: Option<String>,
    /// Answer the privacy questions with the most restrictive choices.
    #[serde(default)]
    pub disable_data_collection: bool,
    /// Prevent automatic BitLocker device encryption.
    #[serde(default)]
    pub disable_bitlocker: bool,
}

impl Customization {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// Short human descriptions of the enabled options, for reports and logs.
    pub fn summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.bypass_requirements {
            out.push("Skip TPM / Secure Boot / RAM requirement checks".into());
        }
        if self.bypass_online_account {
            out.push("Allow setup without a Microsoft account".into());
        }
        if let Some(name) = &self.local_account {
            out.push(format!("Create local account \"{name}\""));
        }
        if let Some(locale) = &self.regional {
            out.push(format!("Regional settings {locale}"));
        }
        if self.disable_data_collection {
            out.push("Decline optional data collection".into());
        }
        if self.disable_bitlocker {
            out.push("Disable automatic BitLocker encryption".into());
        }
        out
    }
}

/// Windows local account name rules (`net user` / SAM): 1–20 characters, none
/// of `"/\[]:;|=,+*?<>@`, not only dots/spaces, and not a reserved name.
pub fn validate_account(name: &str) -> Result<()> {
    ensure!(
        (1..=20).contains(&name.chars().count()),
        "account name must be 1 to 20 characters"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || "\"/\\[]:;|=,+*?<>@".contains(c)),
        "account name contains a character Windows does not allow"
    );
    ensure!(
        !name.chars().all(|c| c == '.' || c == ' '),
        "account name cannot consist only of dots or spaces"
    );
    let lower = name.to_ascii_lowercase();
    ensure!(
        !["administrator", "guest", "defaultaccount", "system", "none"].contains(&lower.as_str()),
        "{name} is reserved by Windows"
    );
    Ok(())
}

/// Validate a locale tag such as `en-US`, `pt-BR` or `zh-Hant-TW`.
pub fn validate_locale(tag: &str) -> Result<()> {
    let parts: Vec<_> = tag.split('-').collect();
    ensure!(
        (2..=3).contains(&parts.len())
            && parts
                .iter()
                .all(|p| (2..=4).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
            && parts[0].bytes().all(|b| b.is_ascii_lowercase()),
        "regional setting must look like en-US"
    );
    Ok(())
}

/// The desktop user's locale from `LC_ALL`, `LC_MESSAGES` or `LANG`
/// (`fr_FR.UTF-8` → `fr-FR`). `C`/`POSIX` give no locale.
pub fn host_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .and_then(|v| locale_from_posix(&v))
}

pub fn locale_from_posix(value: &str) -> Option<String> {
    let base = value.split(['.', '@']).next()?;
    let (lang, region) = base.split_once('_')?;
    let tag = format!(
        "{}-{}",
        lang.to_ascii_lowercase(),
        region.to_ascii_uppercase()
    );
    validate_locale(&tag).ok().map(|_| tag)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Map ForgeUSB EFI architecture names to Windows `processorArchitecture`.
fn windows_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        "x86" => Some("x86"),
        _ => None,
    }
}

fn component(name: &str, arch: &str, body: &str) -> String {
    format!(
        "    <component name=\"{name}\" processorArchitecture=\"{arch}\" publicKeyToken=\"31bf3856ad364e35\" language=\"neutral\" versionScope=\"nonSxS\" xmlns:wcm=\"http://schemas.microsoft.com/WMIConfig/2002/State\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n{body}    </component>\n"
    )
}

fn commands(element: &str, item: &str, list: &[String]) -> String {
    let mut body = format!("      <{element} wcm:action=\"add\">\n");
    for (i, command) in list.iter().enumerate() {
        body += &format!(
            "        <{item} wcm:action=\"add\">\n          <Order>{}</Order>\n          <Path>{}</Path>\n        </{item}>\n",
            i + 1,
            escape(command)
        );
    }
    body + &format!("      </{element}>\n")
}

fn reg(key: &str, value: &str) -> String {
    format!("reg add \"{key}\" /v {value} /t REG_DWORD /d 1 /f")
}

/// Render the answer file, or `None` when no option is enabled.
/// `architectures` are the EFI architectures found on the media.
pub fn render(c: &Customization, architectures: &[String]) -> Result<Option<String>> {
    if c.is_empty() {
        return Ok(None);
    }
    if let Some(name) = &c.local_account {
        validate_account(name)?;
    }
    if let Some(tag) = &c.regional {
        validate_locale(tag)?;
    }
    let mut archs: Vec<&str> = architectures
        .iter()
        .filter_map(|a| windows_arch(a))
        .collect();
    archs.sort_unstable();
    archs.dedup();
    ensure!(
        !archs.is_empty(),
        "no supported Windows architecture found for the answer file"
    );
    let mut pe = String::new();
    let mut specialize = String::new();
    let mut oobe = String::new();
    for arch in archs {
        let mut pe_commands = Vec::new();
        if c.bypass_requirements {
            for value in ["BypassTPMCheck", "BypassSecureBootCheck", "BypassRAMCheck"] {
                pe_commands.push(reg("HKLM\\SYSTEM\\Setup\\LabConfig", value));
            }
        }
        if !pe_commands.is_empty() {
            pe += &component(
                "Microsoft-Windows-Setup",
                arch,
                &commands("RunSynchronous", "RunSynchronousCommand", &pe_commands),
            );
        }
        let mut spec_commands = Vec::new();
        if c.bypass_online_account || c.local_account.is_some() {
            spec_commands.push(reg(
                "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\OOBE",
                "BypassNRO",
            ));
        }
        if c.disable_bitlocker {
            spec_commands.push(reg(
                "HKLM\\SYSTEM\\CurrentControlSet\\Control\\BitLocker",
                "PreventDeviceEncryption",
            ));
        }
        if !spec_commands.is_empty() {
            specialize += &component(
                "Microsoft-Windows-Deployment",
                arch,
                &commands("RunSynchronous", "RunSynchronousCommand", &spec_commands),
            );
        }
        if let Some(tag) = &c.regional {
            let tag = escape(tag);
            oobe += &component(
                "Microsoft-Windows-International-Core",
                arch,
                &format!(
                    "      <InputLocale>{tag}</InputLocale>\n      <SystemLocale>{tag}</SystemLocale>\n      <UserLocale>{tag}</UserLocale>\n"
                ),
            );
        }
        let mut shell = String::new();
        if c.bypass_online_account || c.disable_data_collection || c.local_account.is_some() {
            shell += "      <OOBE>\n";
            if c.bypass_online_account || c.local_account.is_some() {
                shell += "        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>\n";
            }
            if c.disable_data_collection {
                shell += "        <ProtectYourPC>3</ProtectYourPC>\n";
            }
            shell += "      </OOBE>\n";
        }
        if let Some(name) = &c.local_account {
            let name = escape(name);
            shell += &format!(
                "      <UserAccounts>\n        <LocalAccounts>\n          <LocalAccount wcm:action=\"add\">\n            <Name>{name}</Name>\n            <Group>Administrators</Group>\n            <Password>\n              <Value></Value>\n              <PlainText>true</PlainText>\n            </Password>\n          </LocalAccount>\n        </LocalAccounts>\n      </UserAccounts>\n"
            );
            shell += &commands(
                "FirstLogonCommands",
                "SynchronousCommand",
                &[
                    format!(
                        "net user \"{}\" /logonpasswordchg:yes",
                        c.local_account.as_deref().unwrap_or_default()
                    ),
                    "net accounts /maxpwage:unlimited".into(),
                ],
            )
            .replace("<Path>", "<CommandLine>")
            .replace("</Path>", "</CommandLine>");
        }
        if !shell.is_empty() {
            oobe += &component("Microsoft-Windows-Shell-Setup", arch, &shell);
        }
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- Generated by ForgeUSB. Remove this file to run an unmodified Windows Setup. -->\n<unattend xmlns=\"urn:schemas-microsoft-com:unattend\">\n",
    );
    for (pass, body) in [
        ("windowsPE", pe),
        ("specialize", specialize),
        ("oobeSystem", oobe),
    ] {
        if !body.is_empty() {
            xml += &format!("  <settings pass=\"{pass}\">\n{body}  </settings>\n");
        }
    }
    xml += "</unattend>\n";
    Ok(Some(xml))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balanced(xml: &str) -> bool {
        // Minimal well-formedness: every opened element closes in order.
        let mut stack = Vec::new();
        let mut rest = xml;
        while let Some(i) = rest.find('<') {
            rest = &rest[i + 1..];
            let end = rest.find('>').unwrap();
            let tag = &rest[..end];
            rest = &rest[end + 1..];
            if tag.starts_with('?') || tag.starts_with('!') || tag.ends_with('/') {
                continue;
            }
            let name = tag
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap();
            if tag.starts_with('/') {
                if stack.pop() != Some(name.to_owned()) {
                    return false;
                }
            } else {
                stack.push(name.to_owned());
            }
        }
        stack.is_empty()
    }

    #[test]
    fn empty_customization_writes_nothing() {
        assert!(
            render(&Customization::default(), &["x86_64".into()])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn renders_all_options_per_architecture() {
        let c = Customization {
            bypass_requirements: true,
            bypass_online_account: true,
            local_account: Some("Ana & Bo".into()),
            regional: Some("fr-FR".into()),
            disable_data_collection: true,
            disable_bitlocker: true,
        };
        let xml = render(&c, &["x86_64".into(), "aarch64".into()])
            .unwrap()
            .unwrap();
        assert!(balanced(&xml), "{xml}");
        for needle in [
            "BypassTPMCheck",
            "BypassSecureBootCheck",
            "BypassRAMCheck",
            "BypassNRO",
            "PreventDeviceEncryption",
            "<ProtectYourPC>3</ProtectYourPC>",
            "<Name>Ana &amp; Bo</Name>",
            "<SystemLocale>fr-FR</SystemLocale>",
            "processorArchitecture=\"amd64\"",
            "processorArchitecture=\"arm64\"",
            "logonpasswordchg:yes",
        ] {
            assert!(xml.contains(needle), "missing {needle}");
        }
        assert!(!xml.contains("<Path>net user"));
        assert_eq!(c.summary().len(), 6);
    }

    #[test]
    fn validates_names_and_locales() {
        assert!(validate_account("nidall").is_ok());
        for bad in ["", "a/b", "Administrator", "...", "x".repeat(21).as_str()] {
            assert!(validate_account(bad).is_err(), "{bad}");
        }
        assert_eq!(locale_from_posix("fr_FR.UTF-8").as_deref(), Some("fr-FR"));
        assert_eq!(locale_from_posix("ar_DZ.UTF-8@x").as_deref(), Some("ar-DZ"));
        assert_eq!(locale_from_posix("C.UTF-8"), None);
        assert!(validate_locale("en-US").is_ok());
        assert!(validate_locale("EN").is_err());
    }
}
