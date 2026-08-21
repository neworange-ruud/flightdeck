//
//  ProjectsSearch.swift
//  FlightDeckRemote
//
//  Client-side project search (PRD §5.2: "search affordance top-right"; v1
//  filters the list by name). Pure and unit-tested.
//

import Foundation

enum ProjectsSearch {
    /// Case-insensitive substring match on project name. An empty/whitespace
    /// query returns every project unfiltered.
    static func filter(projects: [Wire.ProjectState], query: String) -> [Wire.ProjectState] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return projects }
        return projects.filter {
            $0.name.range(of: trimmed, options: .caseInsensitive) != nil
        }
    }
}
