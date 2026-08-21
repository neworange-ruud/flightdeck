//
//  ScenePhaseTransportGate.swift
//  FlightDeckRemote
//
//  Pure decision for how a `scenePhase` transition should drive the transport
//  coordinator's foreground/background lifecycle (remote-control-0ef.3).
//
//  Previously `MainTabView` treated anything != `.active` as background and tore
//  the transport down. But iOS enters the transient `.inactive` phase for
//  routine interruptions — a Control Center / Notification Center pull, an
//  app-switcher glance, an incoming-call banner, a Face ID / system prompt —
//  none of which mean the app left the foreground. Tearing down on `.inactive`
//  caused a full reconnect (new WS + hello/auth + resume + snapshot) on every
//  such glance: battery, latency, and re-auth churn.
//
//  Only `.background` is a real teardown; only `.active` is a real connect;
//  `.inactive` is left untouched. This mirrors `RootView`, which arms the app
//  lock on `.background` alone.
//

import SwiftUI

enum ScenePhaseTransportGate {
    /// Maps a scene phase to the transport's foreground intent:
    ///  - `.active`     → `true`  (connect every paired machine)
    ///  - `.background` → `false` (tear every client down; APNs push takes over)
    ///  - `.inactive`   → `nil`   (transient — keep the transport exactly as-is)
    static func foregroundIntent(for phase: ScenePhase) -> Bool? {
        switch phase {
        case .active: return true
        case .background: return false
        case .inactive: return nil
        @unknown default: return nil
        }
    }
}
