//
//  ShellTerminalView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.4 Shell terminal — v1 minimal terminal (streamed
//  stdout/stderr, basic ANSI colors, scrollback, Ctrl-C, copy/paste) backed
//  by SwiftTerm. The Shell feature team fills this in.
//

import SwiftUI

struct ShellTerminalView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "terminal")
                .font(.system(size: 48))
                .foregroundStyle(Theme.textMuted)
            Text("Shell")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Shell terminal placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ShellTerminalView")
    }
}

#Preview {
    ShellTerminalView()
}
