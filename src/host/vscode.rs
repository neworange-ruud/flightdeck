//! "Open Worktree in VS Code": the `vscode://` link that opens a session's
//! folder in the VS Code installed on *this* machine.
//!
//! A local folder is `vscode://file/<path>`. A folder on a FlightDeck this
//! desktop controls remotely is opened through VS Code's Remote - SSH
//! extension, `vscode://vscode-remote/ssh-remote+<target>/<path>`, so VS
//! Code's server runs on the host and the terminal, git and language servers
//! work where the files are. FlightDeck adds nothing to the host for this:
//! SSH carries it, and SSH does its own authentication.
//!
//! The link is handed to the platform's URL handler by the front-end, so
//! nothing here spawns anything.

use std::path::Path;

use crate::web::protocol::HostMachine;

/// The link that opens local folder `path` in VS Code.
pub fn local_folder_url(path: &Path) -> String {
    let os = std::env::consts::OS;
    format!("vscode://file{}", url_path(&path.display().to_string(), os))
}

/// The link that opens folder `path` on a remote host in VS Code over SSH.
/// `host_os` is the host's `std::env::consts::OS`, which decides how its path
/// is spelt in the link.
pub fn remote_folder_url(ssh_target: &str, host_os: &str, path: &str) -> String {
    format!(
        "vscode://vscode-remote/ssh-remote+{}{}",
        ssh_authority(ssh_target),
        url_path(path, host_os)
    )
}

/// The part of the authority after `ssh-remote+`. A plain lowercase
/// `user@host` or alias as it is; anything else — an IPv6 address, whose
/// colons Remote - SSH reads as a port, or an uppercase alias, which VS Code
/// lowercases as a URI host — as Remote - SSH's own escape: the hex of
/// `{"hostName": …, "user": …}`.
fn ssh_authority(target: &str) -> String {
    let plain = target
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._@".contains(&b));
    if plain {
        return target.to_string();
    }
    let spec = match target.rsplit_once('@') {
        Some((user, host)) => serde_json::json!({ "hostName": host, "user": user }),
        None => serde_json::json!({ "hostName": target }),
    };
    spec.to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The SSH destination for a remote at `address` (`host:port`, as saved):
/// the user's own setting when there is one, else the account the host's
/// FlightDeck runs as at the address this desktop reached it on.
pub fn ssh_target(configured: Option<&str>, address: &str, host: Option<&HostMachine>) -> String {
    if let Some(target) = configured.map(str::trim).filter(|t| !t.is_empty()) {
        return target.to_string();
    }
    let name = ssh_host(address);
    match host.and_then(|h| h.user.as_deref()) {
        Some(user) => format!("{user}@{name}"),
        None => name.to_string(),
    }
}

/// The host name in a saved `host:port` address, without the port (and
/// without the brackets around an IPv6 address).
pub fn ssh_host(address: &str) -> &str {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match address.rsplit_once(':') {
        Some((host, _)) if !host.contains(':') => host,
        _ => address,
    }
}

/// `path` as a URL path: forward slashes, a leading `/` (a Windows drive path
/// becomes `/C:/…`), and anything outside the safe set percent-encoded.
fn url_path(path: &str, os: &str) -> String {
    let path = if os == "windows" {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    encode(&path, PATH_SAFE)
}

/// Bytes kept as they are in a path, besides ASCII letters and digits.
const PATH_SAFE: &str = "-._~/:";

/// Percent-encode every byte of `text` that is not an ASCII letter or digit or
/// in `safe`. Deliberately strict: whatever opens the link never sees a space,
/// `%`, `&` or quote it could reinterpret.
fn encode(text: &str, safe: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || safe.as_bytes().contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(user: Option<&str>, os: &str) -> HostMachine {
        HostMachine {
            user: user.map(str::to_string),
            os: os.to_string(),
        }
    }

    #[test]
    fn a_unix_host_folder_opens_over_ssh_as_the_hosts_user() {
        let target = ssh_target(
            None,
            "100.73.111.96:7420",
            Some(&machine(Some("ruud"), "linux")),
        );
        assert_eq!(target, "ruud@100.73.111.96");
        assert_eq!(
            remote_folder_url(
                &target,
                "linux",
                "/home/ruud/web/.flightdeck/worktrees/fix login"
            ),
            "vscode://vscode-remote/ssh-remote+ruud@100.73.111.96\
             /home/ruud/web/.flightdeck/worktrees/fix%20login"
        );
    }

    #[test]
    fn a_windows_host_path_is_spelt_with_a_drive_and_forward_slashes() {
        assert_eq!(
            remote_folder_url("dev@box", "windows", r"C:\Users\dev\My Repo"),
            "vscode://vscode-remote/ssh-remote+dev@box/C:/Users/dev/My%20Repo"
        );
    }

    #[test]
    fn the_users_own_target_wins_and_blank_counts_as_unset() {
        let host = machine(Some("ruud"), "macos");
        assert_eq!(
            ssh_target(Some("buildbox"), "10.0.0.2:7420", Some(&host)),
            "buildbox"
        );
        assert_eq!(
            ssh_target(Some("  "), "10.0.0.2:7420", Some(&host)),
            "ruud@10.0.0.2"
        );
    }

    #[test]
    fn an_older_host_that_names_no_user_leaves_it_to_ssh() {
        assert_eq!(
            ssh_target(None, "box.tail1234.ts.net:7420", None),
            "box.tail1234.ts.net"
        );
    }

    #[test]
    fn the_port_and_ipv6_brackets_are_not_part_of_the_ssh_host() {
        assert_eq!(ssh_host("[fd7a:115c:a1e0::1]:7420"), "fd7a:115c:a1e0::1");
        assert_eq!(ssh_host("host:7420"), "host");
        assert_eq!(ssh_host("host"), "host");
        assert_eq!(ssh_host("fd7a::1"), "fd7a::1");
    }

    #[test]
    fn nothing_a_url_handler_could_reinterpret_survives_unencoded() {
        let url = remote_folder_url("a b", "linux", "/x/100% & \"y\"#?");
        assert!(!url.contains([' ', '&', '"', '#', '?']), "{url}");
        assert!(url.ends_with("/x/100%25%20%26%20%22y%22%23%3F"), "{url}");
    }

    fn unhex(hex: &str) -> serde_json::Value {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Remote - SSH reads an IPv6 address's colons as a port, and VS Code
    /// lowercases an authority's host, so both go as its hex escape.
    #[test]
    fn an_ipv6_host_or_uppercase_alias_goes_as_the_hex_escape() {
        let url = remote_folder_url("ruud@fd7a:115c::1", "linux", "/repo");
        let hex = url
            .strip_prefix("vscode://vscode-remote/ssh-remote+")
            .and_then(|rest| rest.strip_suffix("/repo"))
            .unwrap();
        assert!(hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(
            unhex(hex),
            serde_json::json!({ "hostName": "fd7a:115c::1", "user": "ruud" })
        );

        let url = remote_folder_url("BuildBox", "linux", "/repo");
        let hex = &url["vscode://vscode-remote/ssh-remote+".len()..url.len() - "/repo".len()];
        assert_eq!(unhex(hex), serde_json::json!({ "hostName": "BuildBox" }));

        assert!(remote_folder_url("dev@box-1.lan", "linux", "/r")
            .starts_with("vscode://vscode-remote/ssh-remote+dev@box-1.lan/"));
    }

    #[test]
    fn a_local_folder_uses_the_file_authority() {
        if cfg!(windows) {
            assert_eq!(
                local_folder_url(Path::new(r"C:\src\web")),
                "vscode://file/C:/src/web"
            );
        } else {
            assert_eq!(
                local_folder_url(Path::new("/src/my web")),
                "vscode://file/src/my%20web"
            );
        }
    }
}
