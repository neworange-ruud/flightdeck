//
//  GitIndicatorText.swift
//  FlightDeckRemote
//
//  Compact Geist Mono git indicators, e.g. "~3 drift:2", "+12 ~4", "clean",
//  "no-upstream". PRD §11: Geist Mono for "code, terminal, git indicators,
//  and monospaced values."
//

import SwiftUI

struct GitIndicatorText: View {

    /// The kinds of compact git status this renders. `.custom` covers any
    /// pre-formatted string a caller already has.
    enum Kind: Hashable {
        case clean
        case noUpstream
        case diff(added: Int = 0, modified: Int = 0, deleted: Int = 0, drift: Int = 0)
        case custom(String)
    }

    var kind: Kind

    private var text: String {
        switch kind {
        case .clean:
            "clean"
        case .noUpstream:
            "no-upstream"
        case .diff(let added, let modified, let deleted, let drift):
            Self.diffText(added: added, modified: modified, deleted: deleted, drift: drift)
        case .custom(let text):
            text
        }
    }

    private static func diffText(added: Int, modified: Int, deleted: Int, drift: Int) -> String {
        var parts: [String] = []
        if added > 0 { parts.append("+\(added)") }
        if modified > 0 { parts.append("~\(modified)") }
        if deleted > 0 { parts.append("-\(deleted)") }
        if parts.isEmpty { parts.append("clean") }
        if drift > 0 { parts.append("drift:\(drift)") }
        return parts.joined(separator: " ")
    }

    var body: some View {
        Text(text)
            .typography(Typography.monoSmall)
            .foregroundStyle(Theme.textDim)
            .accessibilityIdentifier("git-indicator-text")
    }
}

#Preview {
    VStack(alignment: .leading, spacing: 10) {
        GitIndicatorText(kind: .diff(modified: 3, drift: 2))
        GitIndicatorText(kind: .diff(added: 12, modified: 4))
        GitIndicatorText(kind: .clean)
        GitIndicatorText(kind: .noUpstream)
        GitIndicatorText(kind: .custom("detached @ a1b2c3d"))
    }
    .padding(24)
    .background(Theme.bgDeep)
}
