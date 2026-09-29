//! How this copy of the desktop app was installed, from where it runs.
//!
//! The answer decides what the update banner offers. A copy a package manager
//! owns must not replace itself (the manager would still believe the old
//! version is installed, and the next `brew upgrade` or `apt upgrade` would
//! fight the swap), so it only gets a "update with <manager>" line. A copy the
//! user unpacked or dragged somewhere (an app bundle, an AppImage, a portable
//! zip) is ours to replace, so it gets "Update now".
//!
//! [`detect`] is a pure function of a [`Probe`] (the running executable, the
//! `$APPIMAGE` variable, the Windows Program Files roots) plus the
//! [`FileSystem`] seam for the few marker files it looks at, so every OS's
//! rules are unit-tested on any host.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use flightdeck::contracts::FileSystem;
use flightdeck::tui::platform::{IS_MACOS, IS_WINDOWS};

/// The operating systems a release has assets for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    /// The OS this binary was built for.
    pub fn current() -> Os {
        if IS_MACOS {
            Os::MacOs
        } else if IS_WINDOWS {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    /// The OS word in an asset file name.
    pub fn asset_word(self) -> &'static str {
        match self {
            Os::MacOs => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }
}

/// The CPU architectures a release has assets for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// The architecture this binary was built for, or `None` for one no
    /// release is built for (the updater then only shows a notice).
    pub fn current() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    /// The architecture word in an asset file name (the `uname -m` spelling).
    pub fn asset_word(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }
}

/// A package manager that owns the install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    /// Homebrew cask (`brew install --cask flightdeck-desktop`).
    Homebrew,
    /// A `.deb` installed with dpkg/apt.
    Apt,
    /// An `.rpm` installed with rpm/dnf/zypper.
    Rpm,
    /// The MSI (or winget, which installs the same MSI).
    WindowsInstaller,
}

impl PackageManager {
    /// What to tell the user to run.
    pub fn update_hint(self) -> &'static str {
        match self {
            PackageManager::Homebrew => "brew update && brew upgrade --cask flightdeck-desktop",
            PackageManager::Apt => {
                "sudo apt update && sudo apt install --only-upgrade flightdeck-desktop"
            }
            PackageManager::Rpm => "sudo dnf upgrade flightdeck-desktop",
            PackageManager::WindowsInstaller => {
                "winget upgrade FlightDeck (or run the new .msi from the release page)"
            }
        }
    }
}

/// An install the app may replace itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectKind {
    /// A macOS `.app` bundle outside any package manager's control.
    MacBundle { bundle: PathBuf },
    /// A Linux AppImage; `file` is the `.AppImage` itself.
    AppImage { file: PathBuf },
    /// A Windows portable unzip; `exe` is the running `.exe`.
    WindowsPortable { exe: PathBuf },
}

/// How this copy was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// A package manager owns it: notice only.
    Managed(PackageManager),
    /// The app can update itself.
    Direct(DirectKind),
    /// Somewhere no release ships to (a `cargo run` build, `/usr/local`, an
    /// unrecognised layout): notice only, pointing at the release page.
    Unsupported,
}

/// What [`detect`] looks at.
#[derive(Debug, Clone)]
pub struct Probe {
    pub os: Os,
    /// The running executable (`std::env::current_exe`).
    pub exe: PathBuf,
    /// `$APPIMAGE`: the AppImage runtime sets it to the `.AppImage` file.
    pub appimage: Option<PathBuf>,
    /// Windows: `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%ProgramW6432%`.
    pub program_dirs: Vec<PathBuf>,
}

impl Probe {
    /// The probe for the running process, or `None` when its own path cannot
    /// be determined.
    pub fn from_env() -> Option<Probe> {
        let exe = std::env::current_exe().ok()?;
        let program_dirs = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect();
        Some(Probe {
            os: Os::current(),
            exe,
            appimage: std::env::var_os("APPIMAGE").map(PathBuf::from),
            program_dirs,
        })
    }
}

/// Where Homebrew keeps a cask's receipt directory (Apple silicon, Intel).
const CASKROOMS: [&str; 2] = ["/opt/homebrew/Caskroom", "/usr/local/Caskroom"];
/// The cask's token, from `packaging/homebrew/flightdeck-desktop.rb.tmpl`.
const CASK: &str = "flightdeck-desktop";
/// The `.deb` package name, whose dpkg file list marks an apt-managed install.
const DPKG_LIST: &str = "/var/lib/dpkg/info/flightdeck-desktop.list";

/// Classify the install described by `probe`.
pub fn detect(fs: &dyn FileSystem, probe: &Probe) -> InstallKind {
    match probe.os {
        Os::MacOs => detect_macos(fs, &probe.exe),
        Os::Linux => detect_linux(fs, probe),
        Os::Windows => detect_windows(probe),
    }
}

fn detect_macos(fs: &dyn FileSystem, exe: &Path) -> InstallKind {
    // `<name>.app/Contents/MacOS/flightdeck-desktop`: a bare binary (a cargo
    // build) has no bundle to swap.
    let Some(bundle) = exe
        .ancestors()
        .find(|a| a.extension() == Some(OsStr::new("app")))
    else {
        return InstallKind::Unsupported;
    };
    // A bundle inside a Caskroom is Homebrew's own staging copy.
    let in_caskroom = CASKROOMS.iter().any(|room| bundle.starts_with(room));
    // A cask copies the bundle into /Applications (or ~/Applications) and
    // keeps a receipt directory. The receipt alone is not enough: a stray
    // copy in ~/Downloads is not the cask's.
    let in_applications = bundle
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "Applications");
    let has_receipt = CASKROOMS
        .iter()
        .any(|room| fs.is_dir(&Path::new(room).join(CASK)));
    if in_caskroom || (in_applications && has_receipt) {
        return InstallKind::Managed(PackageManager::Homebrew);
    }
    InstallKind::Direct(DirectKind::MacBundle {
        bundle: bundle.to_path_buf(),
    })
}

fn detect_linux(fs: &dyn FileSystem, probe: &Probe) -> InstallKind {
    // The AppImage runtime exports the image's path; trust it only while the
    // file is there (a stale variable inherited from a parent AppImage is not
    // this process).
    if let Some(image) = &probe.appimage {
        if fs.exists(image) {
            return InstallKind::Direct(DirectKind::AppImage {
                file: image.clone(),
            });
        }
    }
    // Packages install under /usr; /usr/local is a hand copy, which no
    // release asset replaces.
    let packaged = probe.exe.starts_with("/usr") && !probe.exe.starts_with("/usr/local");
    if !packaged {
        return InstallKind::Unsupported;
    }
    if fs.exists(Path::new(DPKG_LIST)) {
        InstallKind::Managed(PackageManager::Apt)
    } else if fs.is_dir(Path::new("/var/lib/rpm")) || fs.is_dir(Path::new("/usr/lib/sysimage/rpm"))
    {
        InstallKind::Managed(PackageManager::Rpm)
    } else {
        InstallKind::Unsupported
    }
}

fn detect_windows(probe: &Probe) -> InstallKind {
    // Windows paths compare case-insensitively and accept either separator.
    let exe = windows_key(&probe.exe);
    let under_program_files = probe.program_dirs.iter().any(|dir| {
        let root = windows_key(dir);
        !root.is_empty() && (exe == root || exe.starts_with(&format!("{root}/")))
    });
    if under_program_files {
        InstallKind::Managed(PackageManager::WindowsInstaller)
    } else {
        InstallKind::Direct(DirectKind::WindowsPortable {
            exe: probe.exe.clone(),
        })
    }
}

/// A Windows path as a comparison key: lower case, `/` separators, no
/// trailing separator.
fn windows_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use flightdeck::testing::FakeFs;

    use super::*;

    fn probe(os: Os, exe: &str) -> Probe {
        Probe {
            os,
            exe: PathBuf::from(exe),
            appimage: None,
            program_dirs: vec![
                PathBuf::from("C:\\Program Files"),
                PathBuf::from("C:\\Program Files (x86)"),
            ],
        }
    }

    const MAC_EXE: &str = "/Applications/FlightDeck.app/Contents/MacOS/flightdeck-desktop";

    #[test]
    fn a_cask_install_in_applications_is_homebrews() {
        let fs = FakeFs::new().with_dir("/opt/homebrew/Caskroom/flightdeck-desktop");
        assert_eq!(
            detect(&fs, &probe(Os::MacOs, MAC_EXE)),
            InstallKind::Managed(PackageManager::Homebrew)
        );
        // The Intel prefix is recognised too.
        let fs = FakeFs::new().with_dir("/usr/local/Caskroom/flightdeck-desktop");
        assert_eq!(
            detect(&fs, &probe(Os::MacOs, MAC_EXE)),
            InstallKind::Managed(PackageManager::Homebrew)
        );
    }

    #[test]
    fn a_bundle_run_from_inside_the_caskroom_is_homebrews() {
        let exe = "/opt/homebrew/Caskroom/flightdeck-desktop/1.0.0/FlightDeck.app/Contents/MacOS/flightdeck-desktop";
        assert_eq!(
            detect(&FakeFs::new(), &probe(Os::MacOs, exe)),
            InstallKind::Managed(PackageManager::Homebrew)
        );
    }

    #[test]
    fn a_dragged_bundle_is_direct_even_when_a_cask_receipt_exists() {
        // The receipt belongs to the copy in /Applications, not to this one.
        let fs = FakeFs::new().with_dir("/opt/homebrew/Caskroom/flightdeck-desktop");
        let exe = "/Users/u/Downloads/FlightDeck.app/Contents/MacOS/flightdeck-desktop";
        assert_eq!(
            detect(&fs, &probe(Os::MacOs, exe)),
            InstallKind::Direct(DirectKind::MacBundle {
                bundle: PathBuf::from("/Users/u/Downloads/FlightDeck.app")
            })
        );
        // No receipt: /Applications is a manual install.
        assert_eq!(
            detect(&FakeFs::new(), &probe(Os::MacOs, MAC_EXE)),
            InstallKind::Direct(DirectKind::MacBundle {
                bundle: PathBuf::from("/Applications/FlightDeck.app")
            })
        );
    }

    #[test]
    fn a_bare_macos_binary_has_nothing_to_swap() {
        assert_eq!(
            detect(
                &FakeFs::new(),
                &probe(Os::MacOs, "/repo/target/debug/flightdeck-desktop")
            ),
            InstallKind::Unsupported
        );
    }

    #[test]
    fn linux_packages_and_appimages() {
        let dpkg = FakeFs::new().with_file(DPKG_LIST, "/usr/bin/flightdeck-desktop\n");
        assert_eq!(
            detect(&dpkg, &probe(Os::Linux, "/usr/bin/flightdeck-desktop")),
            InstallKind::Managed(PackageManager::Apt)
        );
        let rpm = FakeFs::new().with_dir("/var/lib/rpm");
        assert_eq!(
            detect(&rpm, &probe(Os::Linux, "/usr/bin/flightdeck-desktop")),
            InstallKind::Managed(PackageManager::Rpm)
        );
        // Under /usr but no package database says so: not ours to guess.
        assert_eq!(
            detect(
                &FakeFs::new(),
                &probe(Os::Linux, "/usr/bin/flightdeck-desktop")
            ),
            InstallKind::Unsupported
        );
        // A hand copy in /usr/local is not a package.
        assert_eq!(
            detect(
                &dpkg,
                &probe(Os::Linux, "/usr/local/bin/flightdeck-desktop")
            ),
            InstallKind::Unsupported
        );
        // The AppImage runtime mounts the image, so the exe is under /tmp/.mount_*.
        let fs = FakeFs::new().with_file("/home/u/FlightDeck.AppImage", "");
        let mut p = probe(
            Os::Linux,
            "/tmp/.mount_FlightXYZ/usr/bin/flightdeck-desktop",
        );
        p.appimage = Some(PathBuf::from("/home/u/FlightDeck.AppImage"));
        assert_eq!(
            detect(&fs, &p),
            InstallKind::Direct(DirectKind::AppImage {
                file: PathBuf::from("/home/u/FlightDeck.AppImage")
            })
        );
        // A stale $APPIMAGE pointing at a file that is gone is ignored.
        assert_eq!(detect(&FakeFs::new(), &p), InstallKind::Unsupported);
    }

    #[test]
    fn windows_installed_and_portable() {
        let fs = FakeFs::new();
        assert_eq!(
            detect(
                &fs,
                &probe(
                    Os::Windows,
                    "C:\\Program Files\\FlightDeck\\flightdeck-desktop.exe"
                )
            ),
            InstallKind::Managed(PackageManager::WindowsInstaller)
        );
        // Case and separators do not matter on Windows.
        assert_eq!(
            detect(
                &fs,
                &probe(
                    Os::Windows,
                    "c:/program files (x86)/FlightDeck/flightdeck-desktop.exe"
                )
            ),
            InstallKind::Managed(PackageManager::WindowsInstaller)
        );
        // "Program Files Extra" is not "Program Files".
        assert!(matches!(
            detect(
                &fs,
                &probe(
                    Os::Windows,
                    "C:\\Program Files Extra\\flightdeck-desktop.exe"
                )
            ),
            InstallKind::Direct(DirectKind::WindowsPortable { .. })
        ));
        assert_eq!(
            detect(
                &fs,
                &probe(Os::Windows, "D:\\Tools\\FlightDeck\\flightdeck-desktop.exe")
            ),
            InstallKind::Direct(DirectKind::WindowsPortable {
                exe: PathBuf::from("D:\\Tools\\FlightDeck\\flightdeck-desktop.exe")
            })
        );
    }

    #[test]
    fn every_manager_names_its_command() {
        for manager in [
            PackageManager::Homebrew,
            PackageManager::Apt,
            PackageManager::Rpm,
            PackageManager::WindowsInstaller,
        ] {
            assert!(!manager.update_hint().is_empty());
        }
        assert!(PackageManager::Homebrew.update_hint().contains("brew"));
    }
}
