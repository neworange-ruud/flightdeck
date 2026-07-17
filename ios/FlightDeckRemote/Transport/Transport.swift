//
//  Transport.swift
//  FlightDeckRemote
//
//  Group placeholder for the relay transport layer (PRD §9.1): a
//  URLSessionWebSocketTask connection to the hosted relay, per-device
//  identity keypair (Keychain/Secure Enclave), and CryptoKit-based end-to-end
//  encryption of the phone <-> desktop channel. The relay itself is a
//  zero-knowledge blind pipe, so all framing/crypto lives on-device.
//
//  This file exists only to establish the group/module in the project;
//  the Transport feature team fills it in.
//

import Foundation

/// Namespace reserved for the relay transport layer. Intentionally empty.
enum Transport {}
