//
//  Ids.swift
//  FlightDeckRemote
//
//  Swift mirror of `remote/protocol/src/ids.rs`: strongly-typed string
//  identifiers. On the wire every id is a plain JSON string (the Rust side
//  uses `#[serde(transparent)]` newtypes); in Swift they are
//  `RawRepresentable` wrappers around `String` so a `SessionId` can't be
//  passed where a `ProjectId` is expected.
//
//  All wire-protocol types live under the `Wire` namespace to avoid
//  colliding with UI-layer types (e.g. the DesignSystem `AgentStatus` and
//  Navigation `DeepLink`).
//

import Foundation

/// Namespace for the FlightDeck Remote wire-protocol mirror
/// (spec: `specs/REMOTE_PROTOCOL.md`; normative types: `remote/protocol/`).
enum Wire {}

/// A strongly-typed string identifier. Encodes/decodes as a bare JSON string
/// via `RawRepresentable`'s default `Codable` implementation.
protocol WireStringId: RawRepresentable, Codable, Hashable, Sendable,
    CustomStringConvertible where RawValue == String {
    init(rawValue: String)
}

extension WireStringId {
    /// Convenience: wrap a plain string as this id.
    init(_ value: String) { self.init(rawValue: value) }

    var description: String { rawValue }
}

extension Wire {
    /// Identifies one phone <-> Mac pairing. All relay routing is keyed by this.
    struct PairingId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// Stable per-device identity, mapped by the relay to a registered public
    /// key for challenge-response authentication.
    struct DeviceId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// Client-generated id attached to every phone->desktop command. Used for
    /// delivery acknowledgement and idempotent application.
    struct CommandId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// One agent session == one worktree == one branch == one primary agent.
    struct SessionId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// A repository/project folder open in FlightDeck.
    struct ProjectId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// A shell terminal within a session (one at a time per session in v1).
    struct ShellId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// A pending permission prompt awaiting an allow/deny decision.
    struct PromptId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// A status event (finished / needs-input / error) in the activity feed.
    struct EventId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }

    /// A single item in a cleaned transcript feed.
    struct ItemId: WireStringId {
        let rawValue: String
        init(rawValue: String) { self.rawValue = rawValue }
    }
}
