//
//  AppRouter.swift
//  FlightDeckRemote
//
//  Top-level entry-flow routing (PRD §5.8): unpaired devices land on the
//  Pairing screen; paired devices land on Projects. This is deliberately a
//  thin stub — deep-linking (notifications → agent session, PRD §5.2/§5.7)
//  and nested navigation are added by the feature teams on top of this.
//

import SwiftUI

/// The two top-level destinations the app can be in.
enum AppRoute: Equatable {
    case pairing
    case projects
}

/// Chooses the root screen based on pairing state and renders it.
struct AppRouter: View {
    var pairingStore: PairingStore

    /// Internal (not private) so it can be exercised directly in unit tests
    /// without standing up the full view hierarchy.
    var route: AppRoute {
        pairingStore.isPaired ? .projects : .pairing
    }

    var body: some View {
        Group {
            switch route {
            case .pairing:
                PairingView()
            case .projects:
                ProjectsListView()
            }
        }
    }
}

#Preview {
    AppRouter(pairingStore: PairingStore())
}
