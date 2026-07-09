//! Default shell resolution for child terminals (SPECS §19).

/// The user's default shell.
///
/// On Unix this honours `$SHELL`, falling back to `/bin/zsh`. On Windows there
/// is no `$SHELL`; we launch PowerShell — preferring PowerShell 7+ (`pwsh.exe`)
/// when it is on `PATH`, otherwise the in-box Windows PowerShell
/// (`powershell.exe`), which is always present.
pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        if crate::agents::adapter::command_exists("pwsh.exe") {
            "pwsh.exe".to_string()
        } else {
            "powershell.exe".to_string()
        }
    }
    #[cfg(not(windows))]
    {
        match std::env::var("SHELL") {
            Ok(s) if !s.trim().is_empty() => s,
            _ => "/bin/zsh".to_string(),
        }
    }
}

/// The command + args used to launch a child shell (SPECS §19).
pub fn shell_launch() -> (String, Vec<String>) {
    (default_shell(), vec![])
}

/// The shell used for a child terminal that runs inside a container via
/// `podman exec`.
///
/// Containers are always Linux (see `runtime::container`), regardless of the
/// host OS, so a containerized child must use a Linux-native shell rather
/// than [`default_shell`] (which resolves to PowerShell on a Windows host and
/// would not exist inside the container). Existence checks against the
/// container's filesystem can't be performed from the host process, so this
/// always resolves to `/bin/bash`; callers that need a `/bin/sh` fallback
/// should handle it as part of the exec invocation itself.
pub fn container_shell() -> String {
    "/bin/bash".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_launch_uses_default_shell_with_no_args() {
        let (cmd, args) = shell_launch();
        assert_eq!(cmd, default_shell());
        assert!(args.is_empty());
    }

    #[test]
    fn default_shell_is_non_empty() {
        // Regardless of environment, we always resolve to *some* shell path.
        assert!(!default_shell().is_empty());
    }

    #[test]
    fn container_shell_is_linux_native_regardless_of_host() {
        // Containers are always Linux, so this must never resolve to a
        // Windows shell, even when the host is Windows.
        assert_eq!(container_shell(), "/bin/bash");
    }
}
