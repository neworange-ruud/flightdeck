//
//  NetworkPathMonitor.swift
//  FlightDeckRemote
//
//  Network-path monitoring for the transport (remote-control-0ef.22). Nothing
//  in the transport layer previously observed connectivity, so on a cell↔wifi
//  switch or a connectivity-restored event the app just waited out the current
//  reconnect attempt/backoff (up to 60s — `Backoff.capMs`) and might not even
//  notice the now-dead socket. This wraps `NWPathMonitor` behind a small seam
//  so `TransportCoordinator` can force an immediate reconnect the instant the
//  phone regains (or changes) its network path, and so unit tests can inject a
//  no-op instead of touching the real system monitor.
//

import Foundation
import Network

/// Observes the phone's network path. `isSatisfied` reflects whether a usable
/// path exists right now (used by the reconnect UI to distinguish "phone is
/// offline" from "relay unreachable" — remote-control-seo); `onPathChange` fires
/// when the path becomes usable again (or its interface changes), so the
/// coordinator can drop a stale socket and reconnect immediately.
@MainActor
protocol NetworkPathMonitoring: AnyObject {
    /// Whether a usable network path currently exists.
    var isSatisfied: Bool { get }
    /// Invoked on a transition to a satisfied path (connectivity restored) or an
    /// interface change while satisfied (cell↔wifi). The `Bool` is the new
    /// `isSatisfied`. Set by the coordinator; only fired while monitoring.
    var onPathChange: (@MainActor (Bool) -> Void)? { get set }
    /// Begin monitoring (idempotent).
    func start()
    /// Stop monitoring (idempotent).
    func cancel()
}

/// Production `NetworkPathMonitoring` over `NWPathMonitor`. Path updates arrive
/// on a background queue and are hopped to the main actor before mutating state
/// or invoking `onPathChange`.
@MainActor
final class NetworkPathMonitor: NetworkPathMonitoring {
    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "agency.neworange.flightdeck.remote.network-path")
    private(set) var isSatisfied = true
    var onPathChange: (@MainActor (Bool) -> Void)?
    private var started = false
    /// The interface type of the last satisfied path, so a cell↔wifi switch
    /// (both satisfied) is still recognized as a change worth reconnecting on.
    private var lastInterface: NWInterface.InterfaceType?

    func start() {
        guard !started else { return }
        started = true
        monitor.pathUpdateHandler = { [weak self] path in
            let satisfied = path.status == .satisfied
            let interface = path.availableInterfaces.first?.type
            Task { @MainActor [weak self] in
                guard let self else { return }
                let wasSatisfied = self.isSatisfied
                let previousInterface = self.lastInterface
                self.isSatisfied = satisfied
                self.lastInterface = satisfied ? interface : nil
                // Reconnect on connectivity restored, or on an interface switch
                // while still online (e.g. wifi → cell): both can leave the
                // existing socket stranded on a dead route.
                let restored = satisfied && !wasSatisfied
                let switched = satisfied && wasSatisfied && interface != previousInterface
                if restored || switched {
                    self.onPathChange?(satisfied)
                }
            }
        }
        monitor.start(queue: queue)
    }

    func cancel() {
        guard started else { return }
        started = false
        monitor.cancel()
    }
}

/// A no-op monitor: always "online", never fires. The default the coordinator
/// falls back to when no real monitor is injected (unit tests), so nothing
/// touches the system `NWPathMonitor` and no proactive reconnects fire.
@MainActor
final class NoopNetworkPathMonitor: NetworkPathMonitoring {
    var isSatisfied: Bool { true }
    var onPathChange: (@MainActor (Bool) -> Void)?
    func start() {}
    func cancel() {}
}
