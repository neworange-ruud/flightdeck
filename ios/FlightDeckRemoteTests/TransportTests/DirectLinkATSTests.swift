//
//  DirectLinkATSTests.swift
//  FlightDeckRemoteTests
//
//  App Transport Security and the direct link (remote-control-gz0d). The phone
//  dials the Mac at plain `ws://<host>:<port>/remote/v1`, where host is a
//  Tailscale MagicDNS name (`*.ts.net`) or a Tailscale `100.x` address.
//
//  Measured here (the tests run inside the host app, so ATS evaluates the
//  app's real Info.plist): URLSession refuses cleartext to BOTH forms with
//  `NSURLErrorAppTransportSecurityRequiresSecureConnection` (-1022) before a
//  byte is sent — `NSAllowsLocalNetworking` does not cover `100.x`, and an
//  `NSExceptionDomains` key cannot be an IP literal. Rather than relax ATS
//  app-wide (`NSAllowsArbitraryLoads`), the direct link runs on
//  Network.framework (`DirectConnection`), which ATS does not govern. These
//  tests pin both halves: ATS stays strict for URLSession, and the direct-link
//  connector reaches both host forms without tripping it.
//
//  No server and no Tailscale are needed: every probe targets an address that
//  fails (refused / unresolvable / unroutable), and only HOW it fails, and how
//  fast, is asserted.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct DirectLinkATSTests {

    private static let atsBlocked = NSURLErrorAppTransportSecurityRequiresSecureConnection

    /// The `NSURLError` code a cleartext URLSession load of `url` fails with.
    private func urlSessionFailureCode(_ url: URL) async -> Int? {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 2
        configuration.waitsForConnectivity = false
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        do {
            _ = try await session.data(from: url)
            return nil
        } catch {
            return (error as NSError).code
        }
    }

    /// Dial `url` with the production direct-link connector (short timeout)
    /// and return the error it failed with plus how long that took.
    private func directFailure(_ url: URL) async -> (error: Error?, elapsed: Duration) {
        let start = ContinuousClock.now
        do {
            let channel = try await DirectConnection(connectTimeout: .seconds(2)).connect(to: url)
            await channel.close()
            return (nil, ContinuousClock.now - start)
        } catch {
            return (error, ContinuousClock.now - start)
        }
    }

    private func isATSError(_ error: Error?) -> Bool {
        guard let error else { return false }
        let ns = error as NSError
        return ns.domain == NSURLErrorDomain && ns.code == Self.atsBlocked
    }

    // MARK: - ATS is NOT relaxed for URLSession

    /// The app's ATS policy is unchanged by the direct link: URLSession still
    /// refuses cleartext to a MagicDNS name, a `100.x` address and an arbitrary
    /// public host. (If this ever fails, someone relaxed ATS — make sure that
    /// was intended.)
    @Test func urlSessionCleartextStaysBlockedByATS() async throws {
        for raw in ["http://fd-ats-probe.tail0000.ts.net:47800/remote/v1",
                    "http://100.64.0.1:9/remote/v1",
                    "http://example.com/"] {
            let url = try #require(URL(string: raw))
            let code = await urlSessionFailureCode(url)
            #expect(code == Self.atsBlocked, "expected ATS to block \(raw), got \(String(describing: code))")
        }
    }

    // MARK: - The direct-link connector is not subject to ATS

    /// A `100.x` IP literal: dialable without any ATS exception. It fails here
    /// (nothing listens), but on the network, not at ATS — and within the
    /// connect timeout rather than hanging.
    @Test func directConnectionDialsTailscaleIPLiteralWithoutATS() async throws {
        let url = try #require(Wire.DirectEndpoint(host: "100.64.0.1", port: 9).wsURL)
        let (error, elapsed) = await directFailure(url)
        #expect(error != nil)
        #expect(!isATSError(error), "direct link tripped ATS for a 100.x address: \(String(describing: error))")
        #expect(elapsed < .seconds(6), "direct connect did not fail fast: \(elapsed)")
    }

    /// A MagicDNS name: dialable without any ATS exception. The probe name does
    /// not resolve, which must surface as a prompt failure, not an ATS block.
    @Test func directConnectionDialsMagicDNSNameWithoutATS() async throws {
        let url = try #require(Wire.DirectEndpoint(host: "fd-ats-probe.tail0000.ts.net", port: 47800).wsURL)
        let (error, elapsed) = await directFailure(url)
        #expect(error != nil)
        #expect(!isATSError(error), "direct link tripped ATS for a *.ts.net host: \(String(describing: error))")
        #expect(elapsed < .seconds(6), "direct connect did not fail fast: \(elapsed)")
    }

    /// A Mac that is up but not listening (refused) fails immediately, so the
    /// reconnect banner and backoff take over at once.
    @Test func directConnectionFailsFastWhenRefused() async throws {
        let url = try #require(URL(string: "ws://127.0.0.1:1/remote/v1"))
        let (error, elapsed) = await directFailure(url)
        #expect(error != nil)
        #expect(elapsed < .seconds(3), "refused connect took \(elapsed)")
    }

    /// Only the direct-link endpoint shape is ever dialed.
    @Test func directConnectionRejectsNonWebSocketURLs() async throws {
        for raw in ["http://100.64.0.1:47800/remote/v1", "file:///etc/hosts"] {
            let url = try #require(URL(string: raw))
            let (error, _) = await directFailure(url)
            #expect((error as? RelayConnectionError) == .invalidURL, "\(raw) -> \(String(describing: error))")
        }
    }
}
