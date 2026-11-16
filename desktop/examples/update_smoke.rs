//! Run the desktop updater's "Update now" against the live GitHub releases,
//! on a copy of the app you point it at. No window, no banner: the same
//! download, checksum, unpack and swap the app does, so a release can be
//! checked end to end before users click the button.
//!
//! ```text
//! cargo run -p flightdeck-desktop --example update_smoke -- <installed copy> <its version>
//! ```
//!
//! The installed copy is the `.app` bundle (macOS), the `.AppImage` (Linux) or
//! `flightdeck-desktop.exe` (Windows). Use a throwaway copy: it is replaced
//! (macOS, Linux) or gets an update staged beside it (Windows portable). For a
//! copy the MSI installed it only downloads and verifies the new MSI and says
//! where it is; installing that is what the app's "Restart" does.

use std::path::PathBuf;
use std::process::ExitCode;

use flightdeck::contracts::real::RealFs;
use flightdeck_desktop::selfupdate::kind::{detect, Arch, DirectKind, InstallKind, Os, Probe};
use flightdeck_desktop::selfupdate::release::Version;
use flightdeck_desktop::selfupdate::source::{GithubReleases, SystemUnzip};
use flightdeck_desktop::selfupdate::state::update_to_latest;
use flightdeck_desktop::selfupdate::Outcome;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(target), Some(version)) = (args.first(), args.get(1)) else {
        eprintln!("usage: update_smoke <installed copy> <its version>");
        return ExitCode::from(2);
    };
    let Some(current) = Version::parse(version) else {
        eprintln!("not a version: {version}");
        return ExitCode::from(2);
    };
    let target = PathBuf::from(target);
    let os = Os::current();
    // What the app would decide for an executable inside this copy.
    let exe = match os {
        Os::MacOs => target.join("Contents/MacOS/flightdeck-desktop"),
        Os::Linux | Os::Windows => target.clone(),
    };
    let probe = Probe {
        os,
        exe: exe.clone(),
        appimage: (os == Os::Linux).then(|| target.clone()),
        program_dirs: Probe::from_env()
            .map(|p| p.program_dirs)
            .unwrap_or_default(),
        local_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    };
    let kind = detect(&RealFs, &probe);
    println!("install kind: {kind:?}");
    let direct = match kind {
        InstallKind::Direct(direct) => direct,
        other => {
            println!("the app would only show a notice for {other:?}; nothing to install");
            return ExitCode::SUCCESS;
        }
    };
    let Some(arch) = Arch::current() else {
        eprintln!("no release is built for this architecture");
        return ExitCode::FAILURE;
    };
    match update_to_latest(
        &GithubReleases::flightdeck(),
        &RealFs,
        &SystemUnzip { os },
        arch,
        current,
        &direct,
    ) {
        Ok((installed, outcome)) => {
            println!("installed v{installed}: {outcome:?}");
            match (&direct, &outcome) {
                (DirectKind::WindowsPortable { .. }, _) => {
                    println!("start {} once to apply it", exe.display());
                }
                (_, Outcome::RunInstaller { msi }) => {
                    println!("verified; install with: msiexec /i \"{}\"", msi.display());
                }
                _ => {}
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("update failed: {e}");
            ExitCode::FAILURE
        }
    }
}
