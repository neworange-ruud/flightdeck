//! Strongly-typed string identifiers used throughout the protocol.
//!
//! Every id is a transparent newtype over `String` so that on the wire it is
//! just a JSON string, but in Rust the compiler stops a `SessionId` from being
//! passed where a `ProjectId` is expected. The iOS Swift mirror should model
//! these the same way (a `RawRepresentable` wrapper around `String`).

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Wrap an owned or borrowed string as this id.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id! {
    /// Identifies one phone <-> Mac pairing. All relay routing is keyed by this.
    /// A phone may hold several `PairingId`s (one per paired Mac) with no
    /// protocol change; see the multi-Mac note in the spec.
    PairingId
}
string_id! {
    /// Stable per-device identity. The relay maps this to a registered Ed25519
    /// public key for challenge-response authentication.
    DeviceId
}
string_id! {
    /// Client-generated id attached to every phone->desktop command. Used for
    /// delivery acknowledgement and idempotent (at-most-once) application.
    CommandId
}
string_id! {
    /// One agent session == one worktree == one branch == one primary agent.
    SessionId
}
string_id! {
    /// A repository/project folder open in FlightDeck.
    ProjectId
}
string_id! {
    /// A shell terminal within a session (one at a time per session in v1).
    ShellId
}
string_id! {
    /// A pending permission prompt awaiting an allow/deny decision.
    PromptId
}
string_id! {
    /// A status event (finished / needs-input / error) in the activity feed.
    EventId
}
string_id! {
    /// A single item in a cleaned transcript feed.
    ItemId
}
