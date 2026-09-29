//! Releases, asset names and checksums: the pure half of the updater.
//!
//! The naming below is a contract with the release pipeline (also written down
//! in `desktop/PACKAGING.md`): the updater finds its download by *name*, never
//! by guessing, so an asset that is missing or misnamed is a refusal to update,
//! not a wrong download.
//!
//! ```text
//! tag       desktop-v<version>                       desktop-v1.4.0
//! macOS     FlightDeck-<version>-macos-<arch>.zip    (FlightDeck.app at the zip root)
//! Linux     FlightDeck-<version>-linux-<arch>.AppImage
//! Windows   FlightDeck-<version>-windows-<arch>-portable.zip   (flightdeck-desktop.exe at the root)
//! checksum  <asset name>.sha256                      `<64 hex>  <asset name>`
//! ```
//!
//! `<arch>` is `x86_64` or `aarch64`. The `.msi`, `.deb`, `.rpm` and the cask
//! are for package managers and are never downloaded by the app.

use std::fmt;
use std::io::Read;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::kind::{Arch, DirectKind, Os};

/// Release tags of the desktop app. The TUI's releases use plain `v<version>`,
/// so the two products share one repository without sharing "latest".
pub const TAG_PREFIX: &str = "desktop-v";

/// A `major.minor.patch` version. Pre-releases and build metadata are not
/// versions the updater will move to, so [`Version::parse`] refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version(major, minor, patch)
    }

    /// Parse `1.2.3` (a leading `v` is accepted).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

/// A published release of the desktop app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    /// The release page, for "Release notes".
    pub page_url: String,
    pub assets: Vec<ReleaseAsset>,
}

/// The archive kinds the app installs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFormat {
    MacZip,
    AppImage,
    WindowsZip,
}

impl PackageFormat {
    /// The format that replaces `kind`.
    pub fn for_install(kind: &DirectKind) -> PackageFormat {
        match kind {
            DirectKind::MacBundle { .. } => PackageFormat::MacZip,
            DirectKind::AppImage { .. } => PackageFormat::AppImage,
            DirectKind::WindowsPortable { .. } => PackageFormat::WindowsZip,
        }
    }

    /// The OS the format belongs to.
    pub fn os(self) -> Os {
        match self {
            PackageFormat::MacZip => Os::MacOs,
            PackageFormat::AppImage => Os::Linux,
            PackageFormat::WindowsZip => Os::Windows,
        }
    }
}

/// The exact asset name for a version, format and architecture.
pub fn asset_name(version: Version, format: PackageFormat, arch: Arch) -> String {
    let os = format.os().asset_word();
    let arch = arch.asset_word();
    match format {
        PackageFormat::MacZip => format!("FlightDeck-{version}-{os}-{arch}.zip"),
        PackageFormat::AppImage => format!("FlightDeck-{version}-{os}-{arch}.AppImage"),
        PackageFormat::WindowsZip => format!("FlightDeck-{version}-{os}-{arch}-portable.zip"),
    }
}

/// The name of the checksum file published next to `asset`.
pub fn checksum_name(asset: &str) -> String {
    format!("{asset}.sha256")
}

/// The asset and its checksum file, as chosen from a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub asset: ReleaseAsset,
    pub checksum: ReleaseAsset,
}

/// Why a release cannot be installed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    /// The release has no file of the expected name for this OS/arch.
    NoAsset(String),
    /// The asset is there but its `.sha256` is not: refuse rather than install
    /// something unverified.
    NoChecksum(String),
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectError::NoAsset(name) => write!(f, "this release has no {name}"),
            SelectError::NoChecksum(name) => {
                write!(
                    f,
                    "this release has no {name}, so the download cannot be verified"
                )
            }
        }
    }
}

/// Pick the asset for `format`/`arch` and its checksum from `release`.
pub fn select(
    release: &ReleaseInfo,
    format: PackageFormat,
    arch: Arch,
) -> Result<Selected, SelectError> {
    let wanted = asset_name(release.version, format, arch);
    let find = |name: &str| release.assets.iter().find(|a| a.name == name).cloned();
    let asset = find(&wanted).ok_or_else(|| SelectError::NoAsset(wanted.clone()))?;
    let sums = checksum_name(&wanted);
    let checksum = find(&sums).ok_or(SelectError::NoChecksum(sums))?;
    Ok(Selected { asset, checksum })
}

/// The SHA-256 (lower-case hex) a checksum file declares for `asset`.
///
/// Accepts `sha256sum`'s output (`<hex>  <name>` or `<hex> *<name>`, one line
/// per file) and a bare `<hex>`. A line naming another file is not this
/// asset's checksum, so a file published for the wrong asset fails to verify.
pub fn parse_checksum(text: &str, asset: &str) -> Option<String> {
    let is_hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let mut bare = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut fields = line.splitn(2, char::is_whitespace);
        let digest = fields.next()?;
        if !is_hex(digest) {
            return None;
        }
        match fields
            .next()
            .map(|name| name.trim().trim_start_matches('*'))
        {
            Some(name) if name == asset => return Some(digest.to_ascii_lowercase()),
            Some(_) => {}
            None => bare = Some(digest.to_ascii_lowercase()),
        }
    }
    bare
}

/// The SHA-256 of everything `reader` yields, lower-case hex.
pub fn sha256_hex(mut reader: impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The newest published desktop release in a GitHub `releases` API response
/// (a JSON array). Drafts, pre-releases, tags without [`TAG_PREFIX`] and
/// tags that are not a plain version are skipped. `None` when there is no
/// such release; `Err` when the response is not the expected JSON at all.
pub fn latest_release(json: &str) -> Result<Option<ReleaseInfo>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let releases = value
        .as_array()
        .ok_or_else(|| "the releases response is not a list".to_string())?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let newest = releases
        .iter()
        .filter(|r| {
            !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false)
        })
        .filter_map(|r| {
            let tag = text(r, "tag_name")?;
            let version = Version::parse(tag.strip_prefix(TAG_PREFIX)?)?;
            let assets = r["assets"]
                .as_array()?
                .iter()
                .filter_map(|a| {
                    Some(ReleaseAsset {
                        name: text(a, "name")?,
                        url: text(a, "browser_download_url")?,
                    })
                })
                .collect();
            Some(ReleaseInfo {
                version,
                page_url: text(r, "html_url").unwrap_or_default(),
                assets,
            })
        })
        .max_by_key(|release| release.version);
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_parse_strictly_and_order_numerically() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert!(v("1.10.0") > v("1.9.9"), "numeric, not lexicographic");
        for bad in ["1.2", "1.2.3.4", "1.2.3-rc1", "1.x.3", "", "v"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn asset_names_follow_the_published_scheme() {
        let version = v("1.4.0");
        assert_eq!(
            asset_name(version, PackageFormat::MacZip, Arch::Aarch64),
            "FlightDeck-1.4.0-macos-aarch64.zip"
        );
        assert_eq!(
            asset_name(version, PackageFormat::AppImage, Arch::X86_64),
            "FlightDeck-1.4.0-linux-x86_64.AppImage"
        );
        assert_eq!(
            asset_name(version, PackageFormat::WindowsZip, Arch::X86_64),
            "FlightDeck-1.4.0-windows-x86_64-portable.zip"
        );
        assert_eq!(
            checksum_name("FlightDeck-1.4.0-macos-aarch64.zip"),
            "FlightDeck-1.4.0-macos-aarch64.zip.sha256"
        );
    }

    fn release(names: &[&str]) -> ReleaseInfo {
        ReleaseInfo {
            version: v("1.4.0"),
            page_url: "https://example.test/r".into(),
            assets: names
                .iter()
                .map(|name| ReleaseAsset {
                    name: name.to_string(),
                    url: format!("https://example.test/dl/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn selection_takes_this_arch_and_its_checksum() {
        let r = release(&[
            "FlightDeck-1.4.0-macos-x86_64.zip",
            "FlightDeck-1.4.0-macos-x86_64.zip.sha256",
            "FlightDeck-1.4.0-macos-aarch64.zip",
            "FlightDeck-1.4.0-macos-aarch64.zip.sha256",
            "FlightDeck-1.4.0-windows-x86_64.msi",
        ]);
        let picked = select(&r, PackageFormat::MacZip, Arch::Aarch64).unwrap();
        assert_eq!(picked.asset.name, "FlightDeck-1.4.0-macos-aarch64.zip");
        assert_eq!(
            picked.checksum.url,
            "https://example.test/dl/FlightDeck-1.4.0-macos-aarch64.zip.sha256"
        );
    }

    #[test]
    fn a_missing_asset_or_checksum_refuses() {
        let r = release(&["FlightDeck-1.4.0-macos-aarch64.zip"]);
        assert_eq!(
            select(&r, PackageFormat::MacZip, Arch::Aarch64),
            Err(SelectError::NoChecksum(
                "FlightDeck-1.4.0-macos-aarch64.zip.sha256".into()
            ))
        );
        assert!(matches!(
            select(&r, PackageFormat::AppImage, Arch::Aarch64),
            Err(SelectError::NoAsset(_))
        ));
        assert!(matches!(
            select(&r, PackageFormat::MacZip, Arch::X86_64),
            Err(SelectError::NoAsset(_))
        ));
    }

    const HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn checksum_files_in_the_common_spellings_parse() {
        let name = "a.zip";
        assert_eq!(
            parse_checksum(&format!("{HEX}  a.zip\n"), name).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            parse_checksum(&format!("{HEX} *a.zip"), name).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            parse_checksum(&format!("{HEX}\n"), name).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            parse_checksum(&format!("{}  a.zip", HEX.to_uppercase()), name).as_deref(),
            Some(HEX),
            "normalised to lower case"
        );
        // Several files: the line for this asset wins.
        let other = "0".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{other}  b.zip\n{HEX}  a.zip\n"), name).as_deref(),
            Some(HEX)
        );
    }

    #[test]
    fn a_checksum_for_another_file_or_junk_is_refused() {
        assert_eq!(parse_checksum(&format!("{HEX}  b.zip"), "a.zip"), None);
        assert_eq!(parse_checksum("not a checksum  a.zip", "a.zip"), None);
        assert_eq!(
            parse_checksum(&format!("{}  a.zip", &HEX[..63]), "a.zip"),
            None
        );
        assert_eq!(parse_checksum("", "a.zip"), None);
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(sha256_hex(&b"test"[..]).unwrap(), HEX);
        assert_eq!(
            sha256_hex(&b""[..]).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_newest_published_desktop_release_is_picked() {
        let json = r#"[
          {"tag_name":"v9.0.0","draft":false,"prerelease":false,"html_url":"u9","assets":[]},
          {"tag_name":"desktop-v1.5.0-rc1","draft":false,"prerelease":true,"html_url":"rc","assets":[]},
          {"tag_name":"desktop-v1.6.0","draft":true,"prerelease":false,"html_url":"draft","assets":[]},
          {"tag_name":"desktop-v1.3.0","draft":false,"prerelease":false,"html_url":"old","assets":[]},
          {"tag_name":"desktop-v1.4.0","draft":false,"prerelease":false,"html_url":"https://gh/r/1.4.0",
           "assets":[{"name":"FlightDeck-1.4.0-macos-aarch64.zip","browser_download_url":"https://gh/dl/z"},
                     {"name":"broken"}]}
        ]"#;
        let latest = latest_release(json).unwrap().unwrap();
        assert_eq!(latest.version, v("1.4.0"));
        assert_eq!(latest.page_url, "https://gh/r/1.4.0");
        assert_eq!(latest.assets.len(), 1, "an asset without a URL is dropped");
    }

    #[test]
    fn no_desktop_release_is_none_and_garbage_is_an_error() {
        assert_eq!(latest_release("[]"), Ok(None));
        assert_eq!(
            latest_release(r#"[{"tag_name":"v1.0.0","assets":[]}]"#),
            Ok(None)
        );
        assert!(latest_release("<html>").is_err());
        assert!(latest_release(r#"{"message":"rate limited"}"#).is_err());
    }
}
