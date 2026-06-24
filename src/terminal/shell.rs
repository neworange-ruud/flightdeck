//! Default shell resolution for child terminals (SPECS §19).

/// The user's default shell (`$SHELL`, falling back to a sensible default).
pub fn default_shell() -> String {
    todo!("T6: read $SHELL or fall back to /bin/zsh")
}

/// The command + args used to launch a child shell (SPECS §19).
pub fn shell_launch() -> (String, Vec<String>) {
    todo!("T6: (default_shell(), vec![]) — plain interactive shell")
}
