//
//  ConnectionDebugSeam.swift
//  FlightDeckRemote
//
//  DEBUG-only launch-argument seam so UI tests (and developers in the
//  simulator) can force a `RemoteLinkState` deterministically, without a
//  real relay connection. Mirrors the existing `-uitest-reset-pairing`
//  (`PairingStore`) / `-uitest-enable-applock` (`AppLockController`)
//  launch-argument pattern — additive, and scoped to this feature's own
//  files.
//
//  Usage: `-uitest-linkstate <state>`, where `<state>` is one of
//  `disconnected`, `connecting`, `authenticating`, `connected` (0ms), or
//  `connected:<latencyMs>` (e.g. `connected:42`).
//

#if DEBUG
import Foundation

enum ConnectionDebugSeam {
    /// Parses `-uitest-linkstate <state>` out of the given launch arguments,
    /// or `nil` if absent/malformed (in which case callers fall back to the
    /// real `ConnectionStatusSource`).
    static func forcedLinkState(
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> RemoteLinkState? {
        guard let flagIndex = arguments.firstIndex(of: "-uitest-linkstate"),
              arguments.indices.contains(flagIndex + 1)
        else { return nil }
        return parse(arguments[flagIndex + 1])
    }

    static func parse(_ value: String) -> RemoteLinkState? {
        let parts = value.split(separator: ":", maxSplits: 1)
        guard let head = parts.first else { return nil }
        switch head {
        case "disconnected": return .disconnected
        case "connecting": return .connecting
        case "authenticating": return .authenticating
        case "connected":
            let ms = parts.count > 1 ? Int(parts[1]) ?? 0 : 0
            return .connected(latencyMs: ms)
        default: return nil
        }
    }
}
#endif
