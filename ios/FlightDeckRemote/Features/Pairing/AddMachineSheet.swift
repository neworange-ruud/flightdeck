//
//  AddMachineSheet.swift
//  FlightDeckRemote
//
//  Presents the existing pairing handshake (`PairingView` / `RealPairingService`,
//  UNCHANGED) as an "Add machine" flow reachable while already paired
//  (remote-control-b8d.7). `RealPairingService.pair` already APPENDS a
//  `PairedInstance` rather than replacing (remote-control-b8d.4), so completing
//  this sheet while paired with N Macs yields N+1 — the transport coordinator
//  (`MainTabView`'s `.onChange(of: router.pairingStore.instances)`, remote-
//  control-b8d.5) then spins up the new client on its own, no extra wiring
//  needed here.
//
//  Reachable from two entry points (both present this same sheet over the
//  same shared `PairingStore`, so either shows the append immediately):
//   - the feed's toolbar "+" — today `ProjectsListView`'s header, the current
//     stand-in main container until remote-control-b8d.8's unified feed owns
//     its own toolbar;
//   - `SettingsView`'s "Add machine" row.
//
//  This wrapper adds only what a modal needs on top of the full-screen
//  `PairingView`: a dismissible nav bar (title + Cancel) and auto-dismiss on
//  success (`onPaired`). The cap check itself lives in `PairingView` (so it
//  applies identically to onboarding and Add-machine); this sheet doesn't
//  duplicate it.
//

import SwiftUI

struct AddMachineSheet: View {
    var pairingStore: PairingStore

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            PairingView(pairingStore: pairingStore, onPaired: { dismiss() })
                .navigationTitle("Add machine")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { dismiss() }
                            .accessibilityIdentifier("add-machine-cancel-button")
                    }
                }
        }
        .accessibilityIdentifier("AddMachineSheet")
    }
}

#Preview {
    AddMachineSheet(pairingStore: PairingStore())
}
