//
//  FixturesGenerated.swift
//  FlightDeckRemoteTests
//
//  GENERATED FILE — DO NOT EDIT BY HAND.
//  Regenerate with ios/scripts/sync-fixtures.sh whenever
//  remote/protocol/tests/fixtures/ changes.
//
//  Each entry is one golden wire-protocol fixture, base64-encoded verbatim.
//  Categories map to the top-level Swift wire types:
//    relay            -> Wire.RelayFrame
//    desktop_to_phone -> Wire.DesktopToPhone
//    phone_to_desktop -> Wire.PhoneCommand
//
//  remote-control-b8d.9/.10 note: only the new `machine_name` (b8d.9) and
//  `unregister_push_token` (b8d.10) entries below were hand-spliced in (rather
//  than running the full sync script) because `remote/protocol/tests/fixtures/`
//  also picked up `revoke`/`pairing_revoked` and a
//  `pairing_offer.claim_token_hint` field from the SAME upstream commit
//  (7f0cd86) for remote-control-b8d.2, which the iOS `Wire.RelayFrame` mirror
//  doesn't decode yet (that's remote-control-b8d.11's job). Re-run
//  `ios/scripts/sync-fixtures.sh` for a full resync once b8d.11 lands.
//

import Foundation

/// One golden JSON fixture from remote/protocol/tests/fixtures/.
struct ProtocolFixture {
    /// Fixture directory: `relay`, `desktop_to_phone`, or `phone_to_desktop`.
    let category: String
    /// File name without extension, e.g. `pairing_claimed`.
    let name: String
    /// Base64 of the fixture file's exact bytes.
    let base64: String

    /// The fixture's raw JSON bytes.
    var data: Data { Data(base64Encoded: base64)! }
}

enum ProtocolFixtures {
    static let all: [ProtocolFixture] = [
        ProtocolFixture(
            category: "relay",
            name: "ack",
            base64: "ewogICJ0eXBlIjogImFjayIsCiAgInBhaXJpbmdfaWQiOiAicGFpcl9ydXVkX21icCIsCiAgImN1cnNvciI6IDQxCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "auth_challenge",
            base64: "ewogICJ0eXBlIjogImF1dGhfY2hhbGxlbmdlIiwKICAibm9uY2UiOiAibjNyUWIwWnQ1WWM5bVE0YTFYaDJaZz09IiwKICAic2VydmVyX3RpbWVfbXMiOiAxNzUyNDEyODAwNTAwCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "auth_ok",
            base64: "ewogICJ0eXBlIjogImF1dGhfb2siLAogICJwYWlyaW5nX2lkcyI6IFsKICAgICJwYWlyX3J1dWRfbWJwIgogIF0KfQo="),
        ProtocolFixture(
            category: "relay",
            name: "auth_response",
            base64: "ewogICJ0eXBlIjogImF1dGhfcmVzcG9uc2UiLAogICJkZXZpY2VfaWQiOiAiZGV2XzlmM2ExYyIsCiAgInNpZ25hdHVyZSI6ICJNRVVDSVFEZjhrMmIxczBWZDlwUTB4WmEyYzNkNGU1ZjZnN2g4aTlqMGsxbDJtM240bzVwNnE9PSIsCiAgInBhaXJpbmdfaWRzIjogWwogICAgInBhaXJfcnV1ZF9tYnAiCiAgXQp9Cg=="),
        ProtocolFixture(
            category: "relay",
            name: "bye",
            base64: "ewogICJ0eXBlIjogImJ5ZSIsCiAgInJlYXNvbiI6ICJjbGllbnQgcmVxdWVzdGVkIGRpc2Nvbm5lY3QiCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "envelope",
            base64: "ewogICJ0eXBlIjogImVudmVsb3BlIiwKICAicGFpcmluZ19pZCI6ICJwYWlyX3J1dWRfbWJwIiwKICAic2VxIjogNDIsCiAgInNlbmRlciI6ICJkZXNrdG9wIiwKICAic2VudF9hdF9tcyI6IDE3NTI0MTI4MDIwMDAsCiAgIm5vbmNlIjogIlltRnpaVFkwYm05dVkyVXhNak0wTlE9PSIsCiAgImNpcGhlcnRleHQiOiAiM3EyKzd3cTgzdnYzcTIrN3dxODN2djNxMis3d3E4M3Z2M3EyKzd3PSIKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "error",
            base64: "ewogICJ0eXBlIjogImVycm9yIiwKICAiY29kZSI6ICJwZWVyX3VuYXZhaWxhYmxlIiwKICAibWVzc2FnZSI6ICJkZXNrdG9wIGlzIG5vdCBjdXJyZW50bHkgY29ubmVjdGVkOyBmcmFtZXMgd2lsbCBiZSBxdWV1ZWQiLAogICJwYWlyaW5nX2lkIjogInBhaXJfcnV1ZF9tYnAiCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "hello",
            base64: "ewogICJ0eXBlIjogImhlbGxvIiwKICAicHJvdG9jb2xfdmVyc2lvbiI6IDEsCiAgInJvbGUiOiAicGhvbmUiLAogICJkZXZpY2VfaWQiOiAiZGV2XzlmM2ExYyIsCiAgImNsaWVudCI6IHsKICAgICJhcHBfdmVyc2lvbiI6ICIxLjAuMCIsCiAgICAicGxhdGZvcm0iOiAiaW9zIiwKICAgICJvc192ZXJzaW9uIjogIjE4LjIiCiAgfQp9Cg=="),
        ProtocolFixture(
            category: "relay",
            name: "hello_ok",
            base64: "ewogICJ0eXBlIjogImhlbGxvX29rIiwKICAicHJvdG9jb2xfdmVyc2lvbiI6IDEsCiAgInNlcnZlcl90aW1lX21zIjogMTc1MjQxMjgwMDAwMCwKICAiY29ubmVjdGlvbl9pZCI6ICJjb25uXzAxSFpYOFEiCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "machine_name",
            base64: "ewogICJ0eXBlIjogIm1hY2hpbmVfbmFtZSIsCiAgInBhaXJpbmdfaWQiOiAicGFpcl9ydXVkX21icCIsCiAgIm1hY2hpbmVfbmFtZSI6ICJSdXVkJ3MgTWFjQm9vayBQcm8iCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "pairing_claim",
            base64: "ewogICJ0eXBlIjogInBhaXJpbmdfY2xhaW0iLAogICJjbGFpbV90b2tlbiI6ICI0NzI5LVhrOVFhMkxtIiwKICAiZGV2aWNlX2lkIjogImRldl85ZjNhMWMiLAogICJkZXZpY2VfcHVibGljX2tleSI6ICJjUTJwVjhZYjNOejdXdDFSazRYaDZaZzlNbTBBYTVCYjJDYzREZDZFZTg9IiwKICAia2V5X2FncmVlbWVudF9wdWJsaWNfa2V5IjogIkJFeGFtcGxlUGhvbmVLZXlBZ3JlZW1lbnRTZWMxUHViS2V5QmFzZTY0V291bGQ2NUJ5dGVzR2cySGg0Smo2S2s4TGwwTW0yTm40UHA2UXE4UnIwU3MyVHQ0VXU2dj09IiwKICAicm9sZSI6ICJwaG9uZSIKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "pairing_claimed",
            base64: "ewogICJ0eXBlIjogInBhaXJpbmdfY2xhaW1lZCIsCiAgInBhaXJpbmdfaWQiOiAicGFpcl9ydXVkX21icCIsCiAgInBlZXJfZGV2aWNlX2lkIjogImRldl9tYWNfN2IyMSIsCiAgInBlZXJfa2V5X2FncmVlbWVudF9wdWJsaWNfa2V5IjogIkJFeGFtcGxlRGVza3RvcEtleUFncmVlbWVudFNlYzFQdWJLZXlCYXNlNjRXb3VsZDY1Qnl0ZXNUdDNVdTVWdjdXdzlYeDFZeTNaejVBYTdCYjlDYzFEZDNFZTVGZjdnPT0iCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "pairing_offer",
            base64: "ewogICJ0eXBlIjogInBhaXJpbmdfb2ZmZXIiLAogICJkZXZpY2VfaWQiOiAiZGV2X21hY183YjIxIiwKICAiZGV2aWNlX3B1YmxpY19rZXkiOiAiQkV4YW1wbGVEZXNrdG9wU2VjMVB1YmxpY0tleUJhc2U2NFdvdWxkQmU2NURlY29kZWRCeXRlc1EycFY4WWIzTno3V3QxUms0WGg2Wmc5TW0wQWE1QmIyQ2M0RGQ2RWU4Zj09IiwKICAia2V5X2FncmVlbWVudF9wdWJsaWNfa2V5IjogIkJFeGFtcGxlRGVza3RvcEtleUFncmVlbWVudFNlYzFQdWJLZXlCYXNlNjRXb3VsZDY1Qnl0ZXNUdDNVdTVWdjdXdzlYeDFZeTNaejVBYTdCYjlDYzFEZDNFZTVGZjdnPT0iLAogICJyb2xlIjogImRlc2t0b3AiCn0K"),
        ProtocolFixture(
            category: "relay",
            name: "pairing_offer_ok",
            base64: "ewogICJ0eXBlIjogInBhaXJpbmdfb2ZmZXJfb2siLAogICJwYWlyaW5nX2lkIjogInBhaXJfcnV1ZF9tYnAiLAogICJjbGFpbV90b2tlbiI6ICI0NzI5LVhrOVFhMkxtIiwKICAiZXhwaXJlc19hdF9tcyI6IDE3NTI0MTI5MjA1MDAKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "peer_presence",
            base64: "ewogICJ0eXBlIjogInBlZXJfcHJlc2VuY2UiLAogICJwYWlyaW5nX2lkIjogInBhaXJfcnV1ZF9tYnAiLAogICJwZWVyIjogImRlc2t0b3AiLAogICJzdGF0ZSI6ICJjb25uZWN0ZWQiLAogICJhdF9tcyI6IDE3NTI0MTI4MDEwMDAKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "ping",
            base64: "ewogICJ0eXBlIjogInBpbmciLAogICJjbGllbnRfdGltZV9tcyI6IDE3NTI0MTI4MDMwMDAKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "pong",
            base64: "ewogICJ0eXBlIjogInBvbmciLAogICJjbGllbnRfdGltZV9tcyI6IDE3NTI0MTI4MDMwMDAsCiAgInNlcnZlcl90aW1lX21zIjogMTc1MjQxMjgwMzA0MAp9Cg=="),
        ProtocolFixture(
            category: "relay",
            name: "push_token_ack",
            base64: "ewogICJ0eXBlIjogInB1c2hfdG9rZW5fYWNrIiwKICAicGFpcmluZ19pZCI6ICJwYWlyX3J1dWRfbWJwIgp9Cg=="),
        ProtocolFixture(
            category: "relay",
            name: "register_push_token",
            base64: "ewogICJ0eXBlIjogInJlZ2lzdGVyX3B1c2hfdG9rZW4iLAogICJwYWlyaW5nX2lkIjogInBhaXJfcnV1ZF9tYnAiLAogICJ0b2tlbiI6ICI3NDBmNDcwN2JlYmNmNzRmOWI3YzI1ZDQ4ZTMzNTg5NDVmNmFhMDFkYTVkZGIzODc0NjJjN2VhZjYxYmI3OGFkIiwKICAiZW52aXJvbm1lbnQiOiAicHJvZHVjdGlvbiIKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "unregister_push_token",
            base64: "ewogICJ0eXBlIjogInVucmVnaXN0ZXJfcHVzaF90b2tlbiIsCiAgInBhaXJpbmdfaWQiOiAicGFpcl9ydXVkX21icCIKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "resume",
            base64: "ewogICJ0eXBlIjogInJlc3VtZSIsCiAgInBhaXJpbmdfaWQiOiAicGFpcl9ydXVkX21icCIsCiAgImZyb21fc2VxIjogNDEKfQo="),
        ProtocolFixture(
            category: "relay",
            name: "version_incompatible",
            base64: "ewogICJ0eXBlIjogInZlcnNpb25faW5jb21wYXRpYmxlIiwKICAieW91cl92ZXJzaW9uIjogMiwKICAibWluX3N1cHBvcnRlZCI6IDEsCiAgIm1heF9zdXBwb3J0ZWQiOiAxCn0K"),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "command_ack",
            base64: "ewogICJ0eXBlIjogImNvbW1hbmRfYWNrIiwKICAiY29tbWFuZF9pZCI6ICJjbWRfYzBmZmVlMDEiLAogICJvdXRjb21lIjogImFwcGxpZWQiLAogICJtZXNzYWdlIjogInJlcGx5IGRlbGl2ZXJlZCB0byBmaXgtbG9naW4iCn0K"),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "event",
            base64: "ewogICJ0eXBlIjogImV2ZW50IiwKICAiZXZlbnRfaWQiOiAiZXZ0Xzc3ODgiLAogICJraW5kIjogewogICAgInR5cGUiOiAiZmluaXNoZWQiLAogICAgInN1bW1hcnkiOiAiMTggZmlsZXMgY2hhbmdlZCDCtyByZWFkeSB0byBwdXNoIiwKICAgICJmaWxlc19jaGFuZ2VkIjogMTgsCiAgICAicmVhZHlfdG9fcHVzaCI6IHRydWUKICB9LAogICJkZWVwX2xpbmsiOiB7CiAgICAicHJvamVjdF9pZCI6ICJwcm9qX2ZsaWdodGRlY2siLAogICAgInNlc3Npb25faWQiOiAic2Vzc19hZGRfdGVzdHMiLAogICAgIml0ZW1faWQiOiAiaXRlbV8wMDQyIgogIH0sCiAgIm9jY3VycmVkX2F0X21zIjogMTc1MjQxMjgwMDAwMCwKICAidGl0bGUiOiAiYWRkLXRlc3RzIGZpbmlzaGVkIGl0cyB0dXJuIgp9Cg=="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "git_status",
            base64: "ewogICJ0eXBlIjogImdpdF9zdGF0dXMiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAiYnJhbmNoIjogImZsaWdodGRlY2svZml4LWxvZ2luIiwKICAiYmFzZV9icmFuY2giOiAibWFpbiIsCiAgImhhc191cHN0cmVhbSI6IHRydWUsCiAgImFoZWFkIjogMSwKICAiYmVoaW5kIjogMCwKICAiZHJpZnQiOiAyLAogICJmaWxlcyI6IFsKICAgIHsKICAgICAgInBhdGgiOiAic3JjL2F1dGgudHMiLAogICAgICAic3RhdHVzIjogIm1vZGlmaWVkIiwKICAgICAgImFkZGVkX2xpbmVzIjogMTgsCiAgICAgICJyZW1vdmVkX2xpbmVzIjogNAogICAgfSwKICAgIHsKICAgICAgInBhdGgiOiAic3JjL3Nlc3Npb24udHMiLAogICAgICAic3RhdHVzIjogImFkZGVkIiwKICAgICAgImFkZGVkX2xpbmVzIjogMzAsCiAgICAgICJyZW1vdmVkX2xpbmVzIjogMAogICAgfQogIF0KfQo="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "rollup",
            base64: "ewogICJ0eXBlIjogInJvbGx1cCIsCiAgInByb2plY3RzIjogWwogICAgewogICAgICAicHJvamVjdF9pZCI6ICJwcm9qX2ZsaWdodGRlY2siLAogICAgICAicm9sbHVwIjogewogICAgICAgICJkb3QiOiAid29ya2luZyIsCiAgICAgICAgInN1bW1hcnkiOiAiMSBuZWVkcyBpbnB1dCDCtyAxIHdvcmtpbmcgwrcgMyBhZ2VudHMiLAogICAgICAgICJ3b3JraW5nIjogMSwKICAgICAgICAiaWRsZSI6IDEsCiAgICAgICAgIm5lZWRzX2lucHV0IjogMSwKICAgICAgICAibWFudWFsIjogMCwKICAgICAgICAiYWdlbnRfY291bnQiOiAzCiAgICAgIH0KICAgIH0KICBdCn0K"),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "shell_event",
            base64: "ewogICJ0eXBlIjogInNoZWxsX2V2ZW50IiwKICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgInNoZWxsX2lkIjogInNoZWxsXzAxIiwKICAia2luZCI6IHsKICAgICJ0eXBlIjogIm9wZW5lZCIsCiAgICAiY29scyI6IDgwLAogICAgInJvd3MiOiAyNAogIH0KfQo="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "shell_output",
            base64: "ewogICJ0eXBlIjogInNoZWxsX291dHB1dCIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iLAogICJzaGVsbF9pZCI6ICJzaGVsbF8wMSIsCiAgInN0cmVhbSI6ICJzdGRvdXQiLAogICJzZXEiOiA3LAogICJkYXRhIjogIlx1MDAxYlszMm1QQVNTXHUwMDFiWzBtIHNyYy9hdXRoLnRlc3QudHMgKDQyIHRlc3RzKVxuIgp9Cg=="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "snapshot",
            base64: "ewogICJ0eXBlIjogInNuYXBzaG90IiwKICAic2VydmVyX3RpbWVfbXMiOiAxNzUyNDEyODAwMDAwLAogICJwcm9qZWN0cyI6IFsKICAgIHsKICAgICAgInByb2plY3RfaWQiOiAicHJval9mbGlnaHRkZWNrIiwKICAgICAgIm5hbWUiOiAiZmxpZ2h0ZGVjayIsCiAgICAgICJyb2xsdXAiOiB7CiAgICAgICAgImRvdCI6ICJuZWVkc19pbnB1dCIsCiAgICAgICAgInN1bW1hcnkiOiAiMSBuZWVkcyBpbnB1dCDCtyAxIHdvcmtpbmcgwrcgMyBhZ2VudHMiLAogICAgICAgICJ3b3JraW5nIjogMSwKICAgICAgICAiaWRsZSI6IDEsCiAgICAgICAgIm5lZWRzX2lucHV0IjogMSwKICAgICAgICAibWFudWFsIjogMCwKICAgICAgICAiYWdlbnRfY291bnQiOiAzCiAgICAgIH0sCiAgICAgICJzZXNzaW9ucyI6IFsKICAgICAgICB7CiAgICAgICAgICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgICAgICAgICAicHJvamVjdF9pZCI6ICJwcm9qX2ZsaWdodGRlY2siLAogICAgICAgICAgIm5hbWUiOiAiZml4LWxvZ2luIiwKICAgICAgICAgICJhZ2VudF90eXBlIjogImNsYXVkZV9jb2RlIiwKICAgICAgICAgICJzdGF0dXMiOiB7CiAgICAgICAgICAgICJzdGF0ZSI6ICJuZWVkc19pbnB1dCIKICAgICAgICAgIH0sCiAgICAgICAgICAiZ2l0IjogewogICAgICAgICAgICAiYnJhbmNoIjogImZsaWdodGRlY2svZml4LWxvZ2luIiwKICAgICAgICAgICAgImFkZGVkIjogMCwKICAgICAgICAgICAgIm1vZGlmaWVkIjogMywKICAgICAgICAgICAgInJlbW92ZWQiOiAwLAogICAgICAgICAgICAiYWhlYWQiOiAwLAogICAgICAgICAgICAiYmVoaW5kIjogMCwKICAgICAgICAgICAgImRyaWZ0IjogMiwKICAgICAgICAgICAgImhhc191cHN0cmVhbSI6IHRydWUKICAgICAgICAgIH0sCiAgICAgICAgICAicnVubmluZ190aW1lX3NlY3MiOiA1MTIsCiAgICAgICAgICAicGVuZGluZ19xdWVzdGlvbiI6ICJBbGxvdyBybSAtcmYgZGlzdC8gPyIKICAgICAgICB9LAogICAgICAgIHsKICAgICAgICAgICJzZXNzaW9uX2lkIjogInNlc3NfYWRkX3Rlc3RzIiwKICAgICAgICAgICJwcm9qZWN0X2lkIjogInByb2pfZmxpZ2h0ZGVjayIsCiAgICAgICAgICAibmFtZSI6ICJhZGQtdGVzdHMiLAogICAgICAgICAgImFnZW50X3R5cGUiOiAib3BlbmNvZGUiLAogICAgICAgICAgInN0YXR1cyI6IHsKICAgICAgICAgICAgInN0YXRlIjogIndvcmtpbmciCiAgICAgICAgICB9LAogICAgICAgICAgImdpdCI6IHsKICAgICAgICAgICAgImJyYW5jaCI6ICJmbGlnaHRkZWNrL2FkZC10ZXN0cyIsCiAgICAgICAgICAgICJhZGRlZCI6IDEyLAogICAgICAgICAgICAibW9kaWZpZWQiOiA0LAogICAgICAgICAgICAicmVtb3ZlZCI6IDAsCiAgICAgICAgICAgICJhaGVhZCI6IDAsCiAgICAgICAgICAgICJiZWhpbmQiOiAwLAogICAgICAgICAgICAiZHJpZnQiOiAwLAogICAgICAgICAgICAiaGFzX3Vwc3RyZWFtIjogZmFsc2UKICAgICAgICAgIH0sCiAgICAgICAgICAicnVubmluZ190aW1lX3NlY3MiOiA3MywKICAgICAgICAgICJwZW5kaW5nX3F1ZXN0aW9uIjogbnVsbAogICAgICAgIH0KICAgICAgXQogICAgfQogIF0KfQo="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "status_update",
            base64: "ewogICJ0eXBlIjogInN0YXR1c191cGRhdGUiLAogICJ1cGRhdGVzIjogWwogICAgewogICAgICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgICAgICJwcm9qZWN0X2lkIjogInByb2pfZmxpZ2h0ZGVjayIsCiAgICAgICJzdGF0dXMiOiB7CiAgICAgICAgInN0YXRlIjogImlkbGUiCiAgICAgIH0sCiAgICAgICJydW5uaW5nX3RpbWVfc2VjcyI6IDU0MCwKICAgICAgInBlbmRpbmdfcXVlc3Rpb24iOiBudWxsCiAgICB9LAogICAgewogICAgICAic2Vzc2lvbl9pZCI6ICJzZXNzX2FkZF90ZXN0cyIsCiAgICAgICJwcm9qZWN0X2lkIjogInByb2pfZmxpZ2h0ZGVjayIsCiAgICAgICJzdGF0dXMiOiB7CiAgICAgICAgInN0YXRlIjogIm1hbnVhbCIsCiAgICAgICAgImxhYmVsIjogInJldmlld2luZyBieSBoYW5kIgogICAgICB9LAogICAgICAicnVubmluZ190aW1lX3NlY3MiOiBudWxsLAogICAgICAicGVuZGluZ19xdWVzdGlvbiI6IG51bGwKICAgIH0KICBdCn0K"),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "transcript",
            base64: "ewogICJ0eXBlIjogInRyYW5zY3JpcHQiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAiZnJvbV9pbmRleCI6IDAsCiAgInJlcGxhY2UiOiB0cnVlLAogICJpdGVtcyI6IFsKICAgIHsKICAgICAgInR5cGUiOiAidXNlcl9tZXNzYWdlIiwKICAgICAgIml0ZW1faWQiOiAiaXRlbV8wMDAxIiwKICAgICAgInRleHQiOiAiRml4IHRoZSBsb2dpbiByZWRpcmVjdCBsb29wLiIsCiAgICAgICJhdF9tcyI6IDE3NTI0MTI3MDAwMDAKICAgIH0sCiAgICB7CiAgICAgICJ0eXBlIjogImFnZW50X21lc3NhZ2UiLAogICAgICAiaXRlbV9pZCI6ICJpdGVtXzAwMDIiLAogICAgICAidGV4dCI6ICJGb3VuZCBpdDogdGhlIHNlc3Npb24gY29va2llIHdhcyBjbGVhcmVkIGJlZm9yZSB0aGUgcmVkaXJlY3QuIFBhdGNoaW5nIGF1dGgudHMuIiwKICAgICAgImF0X21zIjogMTc1MjQxMjcwNTAwMAogICAgfSwKICAgIHsKICAgICAgInR5cGUiOiAiYWN0aXZpdHkiLAogICAgICAiaXRlbV9pZCI6ICJpdGVtXzAwMDMiLAogICAgICAic3VtbWFyeSI6ICJFZGl0ZWQgYXV0aC50cyArMTgg4oiSNCIsCiAgICAgICJkZXRhaWwiOiBudWxsLAogICAgICAiYm9keSI6ICJAQCAtMTIsNCArMTIsMTggQEAgZXhwb3J0IGZ1bmN0aW9uIGxvZ2luKCkgeyAuLi4gfSIsCiAgICAgICJraW5kIjogImVkaXQiLAogICAgICAiYXRfbXMiOiAxNzUyNDEyNzA2MDAwCiAgICB9LAogICAgewogICAgICAidHlwZSI6ICJhY3Rpdml0eSIsCiAgICAgICJpdGVtX2lkIjogIml0ZW1fMDAwNCIsCiAgICAgICJzdW1tYXJ5IjogIlJhbiBucG0gdGVzdCIsCiAgICAgICJkZXRhaWwiOiAiNDIgcGFzc2VkIiwKICAgICAgImJvZHkiOiBudWxsLAogICAgICAia2luZCI6ICJ0ZXN0IiwKICAgICAgImF0X21zIjogMTc1MjQxMjcyMDAwMAogICAgfSwKICAgIHsKICAgICAgInR5cGUiOiAicGVybWlzc2lvbl9wcm9tcHQiLAogICAgICAiaXRlbV9pZCI6ICJpdGVtXzAwMDUiLAogICAgICAicHJvbXB0X2lkIjogInByb21wdF9hYjEyIiwKICAgICAgImNvbW1hbmQiOiAicm0gLXJmIGRpc3QvIiwKICAgICAgIm9wdGlvbnMiOiBbCiAgICAgICAgewogICAgICAgICAgImNob2ljZSI6ICJhbGxvd19vbmNlIiwKICAgICAgICAgICJsYWJlbCI6ICJBbGxvdyBvbmNlIgogICAgICAgIH0sCiAgICAgICAgewogICAgICAgICAgImNob2ljZSI6ICJkZW55IiwKICAgICAgICAgICJsYWJlbCI6ICJEZW55IgogICAgICAgIH0KICAgICAgXSwKICAgICAgImF0X21zIjogMTc1MjQxMjczMDAwMAogICAgfQogIF0KfQo="),
        ProtocolFixture(
            category: "desktop_to_phone",
            name: "transcript_append",
            base64: "ewogICJ0eXBlIjogInRyYW5zY3JpcHRfYXBwZW5kIiwKICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgImZyb21faW5kZXgiOiA1LAogICJyZXBsYWNlIjogZmFsc2UsCiAgIml0ZW1zIjogWwogICAgewogICAgICAidHlwZSI6ICJhZ2VudF9tZXNzYWdlIiwKICAgICAgIml0ZW1faWQiOiAiaXRlbV8wMDA2IiwKICAgICAgInRleHQiOiAiRGVuaWVkLiBJJ2xsIGxlYXZlIGRpc3QvIGluIHBsYWNlIGFuZCBjbGVhbiBpdCBhbm90aGVyIHdheS4iLAogICAgICAiYXRfbXMiOiAxNzUyNDEyNzQwMDAwCiAgICB9CiAgXQp9Cg=="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "clear_manual_status",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwNyIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTYwMDAsCiAgInR5cGUiOiAiY2xlYXJfbWFudWFsX3N0YXR1cyIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "close_session",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwNSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTQwMDAsCiAgInR5cGUiOiAiY2xvc2Vfc2Vzc2lvbiIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "git_abandon_worktree",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwYSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTkwMDAsCiAgInR5cGUiOiAiZ2l0X2FiYW5kb25fd29ya3RyZWUiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAiY29uZmlybV9uYW1lIjogImZpeC1sb2dpbiIKfQo="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "git_merge_back",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwOSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTgwMDAsCiAgInR5cGUiOiAiZ2l0X21lcmdlX2JhY2siLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIgp9Cg=="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "git_pull_base",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwOCIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTcwMDAsCiAgInR5cGUiOiAiZ2l0X3B1bGxfYmFzZSIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "mark_read",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAxMSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjYwMDAsCiAgInR5cGUiOiAibWFya19yZWFkIiwKICAiZXZlbnRfaWRzIjogWwogICAgImV2dF83Nzg4IiwKICAgICJldnRfNzc4OSIKICBdCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "new_agent",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwMyIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTIwMDAsCiAgInR5cGUiOiAibmV3X2FnZW50IiwKICAicHJvamVjdF9pZCI6ICJwcm9qX2ZsaWdodGRlY2siLAogICJhZ2VudF90eXBlIjogImNvZGV4IiwKICAibmFtZSI6ICJhZGQtcmF0ZS1saW1pdCIsCiAgImJhc2VfYnJhbmNoIjogIm1haW4iLAogICJmaXJzdF90YXNrIjogIkFkZCBhIHJhdGUgbGltaXRlciB0byB0aGUgbG9naW4gZW5kcG9pbnQuIgp9Cg=="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "permission_decision",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwMiIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTEwMDAsCiAgInR5cGUiOiAicGVybWlzc2lvbl9kZWNpc2lvbiIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iLAogICJwcm9tcHRfaWQiOiAicHJvbXB0X2FiMTIiLAogICJjaG9pY2UiOiAiZGVueSIKfQo="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "reply",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwMSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTAwMDAsCiAgInR5cGUiOiAicmVwbHkiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAidGV4dCI6ICJZZXMsIHJ1biBpdC4gVGhlbiByZWJ1aWxkLiIKfQo="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "request_snapshot",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwZiIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjQwMDAsCiAgInR5cGUiOiAicmVxdWVzdF9zbmFwc2hvdCIsCiAgInByb2plY3RfaWQiOiBudWxsCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "request_transcript",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAxMCIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjUwMDAsCiAgInR5cGUiOiAicmVxdWVzdF90cmFuc2NyaXB0IiwKICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgImZyb21faW5kZXgiOiBudWxsCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "restart_agent",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwNCIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTMwMDAsCiAgInR5cGUiOiAicmVzdGFydF9hZ2VudCIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "set_manual_status",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwNiIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MTUwMDAsCiAgInR5cGUiOiAic2V0X21hbnVhbF9zdGF0dXMiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAibGFiZWwiOiAicmV2aWV3aW5nIGJ5IGhhbmQiCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "shell_close",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwZSIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjMwMDAsCiAgInR5cGUiOiAic2hlbGxfY2xvc2UiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAic2hlbGxfaWQiOiAic2hlbGxfMDEiCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "shell_input",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwYyIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjEwMDAsCiAgInR5cGUiOiAic2hlbGxfaW5wdXQiLAogICJzZXNzaW9uX2lkIjogInNlc3NfZml4X2xvZ2luIiwKICAic2hlbGxfaWQiOiAic2hlbGxfMDEiLAogICJkYXRhIjogIm5wbSB0ZXN0XG4iCn0K"),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "shell_interrupt",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwZCIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjIwMDAsCiAgInR5cGUiOiAic2hlbGxfaW50ZXJydXB0IiwKICAic2Vzc2lvbl9pZCI6ICJzZXNzX2ZpeF9sb2dpbiIsCiAgInNoZWxsX2lkIjogInNoZWxsXzAxIgp9Cg=="),
        ProtocolFixture(
            category: "phone_to_desktop",
            name: "shell_open",
            base64: "ewogICJjb21tYW5kX2lkIjogImNtZF8wMDAwMDAwYiIsCiAgImlzc3VlZF9hdF9tcyI6IDE3NTI0MTI4MjAwMDAsCiAgInR5cGUiOiAic2hlbGxfb3BlbiIsCiAgInNlc3Npb25faWQiOiAic2Vzc19maXhfbG9naW4iLAogICJzaGVsbF9pZCI6ICJzaGVsbF8wMSIsCiAgImNvbHMiOiA4MCwKICAicm93cyI6IDI0Cn0K"),
    ]

    /// Number of embedded fixtures, asserted by the conformance test so a
    /// stale FixturesGenerated.swift is caught when new fixtures land.
    static let expectedCount = 49
}
