//
//  WorkingSpinner.swift
//  FlightDeckRemote
//
//  Small animated rotating-arc spinner for the "working" status (PRD §11
//  motion: "spinner (working)"; PRD §4: red, animated).
//

import SwiftUI

struct WorkingSpinner: View {

    var size: CGFloat = 16
    var lineWidth: CGFloat = 2
    var color: Color = Theme.statusWorking

    @State private var isRotating = false

    var body: some View {
        Circle()
            .trim(from: 0, to: 0.72)
            .stroke(color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
            .frame(width: size, height: size)
            .rotationEffect(.degrees(isRotating ? 360 : 0))
            .onAppear {
                withAnimation(.linear(duration: 0.9).repeatForever(autoreverses: false)) {
                    isRotating = true
                }
            }
            .accessibilityLabel(Text("working"))
            .accessibilityIdentifier("working-spinner")
    }
}

#Preview {
    HStack(spacing: 24) {
        WorkingSpinner(size: 14)
        WorkingSpinner(size: 20, lineWidth: 2.5)
        WorkingSpinner(size: 28, lineWidth: 3)
    }
    .padding(40)
    .background(Theme.bgDeep)
}
