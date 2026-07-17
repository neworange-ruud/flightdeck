//
//  BranchSlugTests.swift
//  FlightDeckRemoteTests
//
//  The Swift slugify port must match the desktop's rules exactly — these are
//  the desktop's own test vectors, ported from `src/git/branch.rs`.
//

import XCTest
@testable import FlightDeckRemote

final class BranchSlugTests: XCTestCase {

    // Mirrors `slugify_lowercases_and_hyphenates_spaces_and_punctuation`.
    func testLowercasesAndHyphenatesSpacesAndPunctuation() {
        XCTAssertEqual(BranchSlug.slugify("Fix the Login Bug!"), "fix-the-login-bug")
        XCTAssertEqual(BranchSlug.slugify("Add OAuth2 support"), "add-oauth2-support")
    }

    // Mirrors `slugify_collapses_runs_and_trims`.
    func testCollapsesRunsAndTrims() {
        XCTAssertEqual(BranchSlug.slugify("  Hello___World  "), "hello-world")
        XCTAssertEqual(BranchSlug.slugify("a // b -- c"), "a-b-c")
        XCTAssertEqual(BranchSlug.slugify("---trim---"), "trim")
        XCTAssertEqual(BranchSlug.slugify("UPPER.CASE"), "upper-case")
    }

    // Mirrors `slugify_empty_and_all_punct`.
    func testEmptyAndAllPunctuation() {
        XCTAssertEqual(BranchSlug.slugify(""), "")
        XCTAssertEqual(BranchSlug.slugify("!!!"), "")
    }

    // Mirrors `branch_name_concatenates_prefix_and_slug`.
    func testBranchNameConcatenatesPrefixAndSlug() {
        XCTAssertEqual(BranchSlug.branchName(prefix: "flightdeck/", slug: "fix-login"),
                       "flightdeck/fix-login")
    }

    // The PRD §5.5 example, end to end.
    func testPRDExample() {
        let slug = BranchSlug.slugify("Add rate limit")
        XCTAssertEqual(BranchSlug.branchName(prefix: BranchSlug.defaultPrefix, slug: slug),
                       "flightdeck/add-rate-limit")
    }

    // Non-ASCII alphanumerics are lowercased but kept (desktop rule).
    func testNonASCIIAlphanumericsAreKeptLowercased() {
        XCTAssertEqual(BranchSlug.slugify("Ünïcode Name"), "ünïcode-name")
    }
}
