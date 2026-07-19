# FlightDeck Remote — Wire Protocol (v1)

> **Status:** v1 — implementable. This is the phone ⇄ desktop wire protocol for
> FlightDeck Remote. It is written so three teams can each build from it
> independently: the **relay** service (Azure Container Apps), the **desktop
> bridge** (FlightDeck's new relay client + transcript/status feed), and the
> **iOS** app (which mirrors the Rust types in Swift).
>
> The normative type definitions live in the `flightdeck-remote-protocol` Rust
> crate (`remote/protocol/`). The golden JSON fixtures in
> `remote/protocol/tests/fixtures/` are the **cross-language contract**: any
> implementation must produce and consume JSON byte-compatible with them. When
> this prose and the fixtures disagree, **the fixtures win**.

---

## 1. Design goals & the two-plane model

The protocol is split into two cleanly separated planes that share one WebSocket:

| Plane | Who reads it | Content | Defined in |
|-------|--------------|---------|------------|
| **Relay plane** | The relay **and** the peer | Plaintext, content-free metadata: versioning, auth, pairing, presence, delivery/sequencing, acks, latency, push tokens, errors | `relay::RelayFrame` |
| **E2E plane** | **Only** phone & desktop | The application messages (status, transcript, commands, shell) as end-to-end-encrypted ciphertext | `e2e::DesktopToPhone`, `e2e::PhoneCommand` |

The relay is a **zero-knowledge blind pipe** (PRD §9.1): it authenticates each
endpoint by device key, routes by **pairing ID only**, and never sees agent
content, transcripts, or shell traffic. All application content travels inside
`relay::EncryptedEnvelope`, whose `ciphertext` is opaque to the relay.

**This crate carries types only — it performs no cryptography.** Sealing/opening
the envelope is a separate layer (see §7).

---

## 2. Transport & framing

- **Transport:** a single **WebSocket** connection from each endpoint to the
  relay (`wss://…`). The desktop holds a long-lived outbound connection; the
  phone connects when foregrounded / woken by push.
- **Two roles connect per pairing:** `desktop` and `phone` (`common::Role`).
- **Framing:** one JSON value per WebSocket **text** message. Each message on the
  relay plane is exactly one `RelayFrame`. There is no additional length prefix
  or batching — the WebSocket frame *is* the protocol frame.
- **Encoding:** UTF-8 JSON. Binary values (ciphertext, nonces, public keys,
  signatures, APNs tokens where noted) are carried as **base64 (standard
  alphabet, padded)** or hex strings, called out per field.
- **Size:** endpoints SHOULD keep a single frame under 1 MiB. Large transcripts
  are paginated (`from_index`) and shell output is chunked; there is no single
  giant frame.

---

## 3. Serde / JSON conventions (normative)

These conventions are fixed so the Rust, relay, and Swift sides agree:

1. **Enums are internally tagged** by a discriminator field. For almost all enums
   the tag is `type`; the sole exception is `AgentStatus`, whose tag is `state`
   (it reads naturally as `{"state":"working"}`). No adjacently-tagged or
   externally-tagged enums appear anywhere.
2. **All identifiers, variant names and field names are `snake_case`** on the
   wire (e.g. `claude_code`, `needs_input`, `command_id`).
3. **IDs are JSON strings.** In Rust they are transparent newtypes
   (`PairingId`, `DeviceId`, `CommandId`, `SessionId`, `ProjectId`, plus
   `ShellId`, `PromptId`, `EventId`, `ItemId`). Swift should mirror them as
   `RawRepresentable`/`Codable` string wrappers.
4. **Timestamps are `*_ms` integers**: signed 64-bit Unix epoch **milliseconds**.
5. **Optional fields are always present and explicit.** v1 serializers do **not**
   omit `null` — an absent optional is written as `"field": null`. Consumers must
   accept `null`. (Deserializers also tolerate a missing key as `null`, but
   producers must emit it, per the fixtures.)
6. **Sequence/counter fields are unsigned** (`seq`, `cursor`, `from_seq`,
   `from_index` are `u64`; counts are `u32`).
7. **Newtype enum variants flatten.** `RelayFrame::Envelope(EncryptedEnvelope)`
   and the `DesktopToPhone` payload variants are newtype variants whose inner
   struct fields appear **flattened next to `type`**, not nested under a key.
   Likewise `PhoneCommand` flattens its `CommandBody`.

---

## 4. Versioning & negotiation

Every connection begins with version negotiation on the relay plane.

- Constants (this build): `PROTOCOL_VERSION = 1`, `MIN_SUPPORTED_VERSION = 1`,
  `MAX_SUPPORTED_VERSION = 1`.
- The client opens with `hello { protocol_version, role, device_id, client }`
  advertising its **preferred** version.
- The relay replies with either:
  - `hello_ok { protocol_version, server_time_ms, connection_id }` — the version
    both sides will use, **or**
  - `version_incompatible { your_version, min_supported, max_supported }` — then
    closes the socket.
- **Negotiation rule** (`negotiate_version`): if the peer's preferred version is
  within `[local_min, local_max]`, use it; if it is **higher**, fall back to
  `local_max` (the relay answers `hello_ok` at its own max — forward compatible);
  if it is **lower than `local_min`**, it is incompatible. In a v1 build only a
  preferred version **below 1** yields `version_incompatible`; a future client
  preferring v2 gets clamped to v1.
- The negotiated version governs the whole connection, including how the E2E
  payloads inside envelopes are interpreted. The E2E application messages do not
  carry a second version field in v1 — the relay-plane negotiated version is
  authoritative. (A future major E2E change would bump `PROTOCOL_VERSION`.)

---

## 5. Connection lifecycle & authentication

### 5.1 Normal (already-paired) connect

```
client → relay : hello { protocol_version, role, device_id, client }
relay  → client: hello_ok { protocol_version, server_time_ms, connection_id }
relay  → client: auth_challenge { nonce, server_time_ms }
client → relay : auth_response { device_id, signature, pairing_ids, machine_name? }
relay  → client: auth_ok { pairing_ids }
relay  → client: machine_name { pairing_id, machine_name }         // 0..n (phone only, §5.7)
relay  → client: peer_presence { pairing_id, peer, state, at_ms }   // 0..n
client → relay : resume { pairing_id, from_seq }                    // per pairing
relay  → client: envelope { … }                                     // replayed, then live
```

- **Challenge-response:** the relay sends a random `nonce`; the client signs the
  nonce bytes with its **per-device ECDSA P-256 private key** (held in the iOS
  Keychain / Secure Enclave, or the desktop keystore) and returns the base64
  `signature` plus the `pairing_ids` it wants active. The relay verifies the
  signature against the **registered public key** for that `device_id`. The relay
  stores only device public keys + pairing membership; it holds **no decryption
  keys**.
- **Signature algorithm (normative, v1):** ECDSA over NIST P-256 with SHA-256.
  P-256 (not Ed25519) is chosen because the iPhone's key must be
  **Secure-Enclave-resident**, and the Secure Enclave only supports P-256.
  Encodings, identical for every device:
  - `device_public_key`: base64(standard, padded) of the **X9.63 uncompressed
    SEC1 point** (65 bytes, `0x04 ‖ x ‖ y`) — CryptoKit `x963Representation`,
    Rust `p256::PublicKey::from_sec1_bytes`.
  - `signature`: base64(standard, padded) of the **raw `r ‖ s` form** (64
    bytes) — CryptoKit `ECDSASignature.rawRepresentation`,
    Rust `p256::ecdsa::Signature::from_slice`.
  - Message signed: the exact decoded `nonce` bytes (hashed with SHA-256 as
    part of standard ECDSA signing).
- On failure the relay sends `error { code: auth_failed, … }` and closes.
- A device may activate several `pairing_ids` on one connection (multi-Mac, §10).
- **`machine_name` (desktop only, optional, v1 amendment).** The desktop's
  `auth_response` MAY carry a `machine_name` — the human-readable name of the Mac
  (e.g. `"Ruud's MacBook Pro"`). It is sent on **every** connect, applies to
  **all** `pairing_ids` on that connection (one Mac → one name), and the relay
  forwards it to the phone via `machine_name` frames (§5.7). The phone omits it
  (`null`). Absent/`null` is fully backward compatible: an older desktop simply
  never sets it, the relay forwards nothing, and the phone keeps its prior name.

### 5.2 Pairing bootstrap (first time)

Pairing is initiated on the desktop (Settings → Remote), which displays a
4-digit code and QR. Both encode a short-lived **claim token** and the material
needed to bootstrap the E2E channel.

```
// Desktop side — obtain a claim token to display as the 4-digit code / QR:
desktop → relay : hello { … role: desktop … }
relay   → desktop: hello_ok { … }
relay   → desktop: auth_challenge { nonce, server_time_ms }
desktop → relay : pairing_offer { device_id, device_public_key, key_agreement_public_key, role }
relay   → desktop: pairing_offer_ok { pairing_id, claim_token, expires_at_ms }
desktop → relay : auth_response { … }              // then proceeds as §5.1
…
// Phone side — redeem it (typically moments later, code entered / QR scanned):
phone → relay : hello { … role: phone … }
relay → phone : hello_ok { … }
relay → phone : auth_challenge { nonce, server_time_ms }
phone → relay : pairing_claim { claim_token, device_id, device_public_key, key_agreement_public_key, role }
relay → phone : pairing_claimed { pairing_id, peer_device_id, peer_key_agreement_public_key }
              // (peer_key_agreement_public_key = the desktop's KA key)
              // (or error { code: pairing_claim_rejected } if token bad/expired)
relay → desktop: pairing_claimed { pairing_id, peer_device_id, peer_key_agreement_public_key }
              // notifies the waiting desktop connection that the phone has
              // joined; peer_key_agreement_public_key = the phone's KA key
phone → relay : auth_response { … }   // then proceeds as §5.1
```

- **Where the claim token comes from (v1 amendment).** The desktop mints it with
  `pairing_offer` → `pairing_offer_ok`. The relay creates the `pairing_id`,
  registers the desktop's device public key, and returns a short-TTL, single-use
  `claim_token` (default TTL `CLAIM_TOKEN_TTL_SECS`). `pairing_offer` is sent
  after `hello_ok` and before the desktop's `auth_response`, mirroring how the
  phone's `pairing_claim` self-registers *its* key before auth — so a brand-new
  desktop with no registered key can still bootstrap. (The original §5.2 prose
  only specified the phone's redemption; these two frames close that gap and are
  now part of the normative type set + fixtures.) A desktop that is **already
  authenticated** may also send `pairing_offer` (an on-demand pairing from
  Settings → Remote); the relay then activates and attaches the new pairing on
  that live connection so the phone's later `pairing_claimed` can reach it, with
  no reconnect needed.
- **`claim_token_hint` (4-digit code, v1 amendment).** `pairing_offer` carries an
  optional `claim_token_hint`. When it is present, well-formed (short printable
  ASCII), and **not currently a live token**, the relay issues it verbatim so the
  desktop can display a short, human-typeable **4-digit code**; otherwise the
  relay mints its own random token. Either way the desktop displays the token the
  relay returns in `pairing_offer_ok`, so the two sides never disagree. A 4-digit
  token has only ~10⁴ of entropy, so the relay pins it to a short TTL, single use,
  **and a per-connection `pairing_claim` rate limit** (`MAX_CLAIM_ATTEMPTS_PER_CONN`,
  default 5) that closes the socket on excess — bounding online brute force. The
  low entropy weakens only the **salt** (§7.1), never the key agreement.
- `pairing_claim` **registers the phone's device public key** against the pairing
  and redeems the one-time token. Tokens are short-TTL and single-use.
- **Key-agreement public keys (KA keys).** Both `pairing_offer` and
  `pairing_claim` additionally carry a `key_agreement_public_key`: the P-256
  public point each endpoint contributes to the static-static ECDH that
  bootstraps the E2E channel (§7.1). Same encoding as `device_public_key`
  (base64 standard-padded X9.63 uncompressed SEC1, 65 bytes). The relay stores
  each device's KA key alongside its identity key and hands each endpoint its
  **peer's** KA key back in `pairing_claimed.peer_key_agreement_public_key`
  (the phone receives the desktop's; the desktop notification receives the
  phone's). Public keys are not secret, so carrying them through the relay is
  safe — the relay never holds either private scalar and still derives nothing.
  - **Why a separate key (SE rationale).** On iOS the per-device *identity* key
    (§5.1) is a **Secure-Enclave signing key**: the SE performs ECDSA with it but
    will not expose or apply its scalar for ECDH, so it cannot be used for key
    agreement. iOS therefore MUST generate a **distinct software P-256 key** for
    key agreement and send its public point as `key_agreement_public_key`. On
    desktop the keystore identity key is usable for ECDH, so the desktop **MAY**
    reuse it (i.e. `key_agreement_public_key` == `device_public_key`); it may
    also use a separate key. The two sides are symmetric on the wire regardless.
- The **QR/code also carries the shared secret that bootstraps the E2E channel**
  (PRD §9.1). That secret is consumed by the crypto layer (§7); it is **not** part
  of any relay frame and never transits the relay. The relay only ever sees the
  opaque `claim_token`.
- Pairing **persists until explicitly unpaired** (PRD §9.1); there is no forced
  periodic re-pair in v1.

### 5.3 Presence & reconnect

- `peer_presence` tells each side whether its peer is currently connected. The
  phone uses this to drive the honest "Reconnecting…" banner (PRD §5.6) and to
  **pause commands** when the desktop is absent.
- On reconnect the client re-runs `hello`/auth, then issues `resume` per pairing
  (§6.3). Nothing is sent blind: if presence shows the peer down, the phone
  refuses to send commands and surfaces the paused state.

### 5.4 Ping / pong (latency)

`ping { client_time_ms }` → `pong { client_time_ms, server_time_ms }`. The phone
displays round-trip latency from `now - client_time_ms`; `server_time_ms` gives
coarse clock-skew awareness. This measures the phone↔relay leg; end-to-end health
also depends on `peer_presence`.

### 5.5 Push-token registration

`register_push_token { pairing_id, token, environment }` → `push_token_ack`.
The APNs token is **opaque and outside E2E** — the relay/desktop use it to drive
notifications when an agent finishes or needs input (PRD §9.1). `environment` is
`sandbox` | `production`.

**Deregistration (per-machine push mute).** A phone can drop a pairing's token
**without unpairing** so it stops that pairing's pushes while keeping the pairing:

```
phone → relay : unregister_push_token { pairing_id }
relay → phone : push_token_ack { pairing_id }   // shared ack, confirms removal
```

- **Membership check (mandatory).** The relay **must** verify the authenticated
  `device_id` is a member of `pairing_id` before removing anything. A
  **non-member** (or an unknown pairing, which has no members) is refused with
  `error { code: unknown_pairing, pairing_id }` and changes nothing — the same
  invariant `revoke` (§5.8) enforces.
- **Idempotent.** Unregistering when no token is stored is a **success no-op**:
  the relay still replies `push_token_ack { pairing_id }` and never errors.
- **Scope.** Only this pairing's token is cleared; the pairing itself and the
  device's other pairings are untouched (unlike `revoke`, which deletes the
  pairing). `push_token_ack` is reused as the confirmation for both
  `register_push_token` and `unregister_push_token`.

### 5.6 Errors & shutdown

- `error { code, message, pairing_id? }` — machine-readable `RelayErrorCode`
  (`unsupported_version`, `auth_failed`, `unknown_pairing`, `not_authenticated`,
  `pairing_claim_rejected`, `peer_unavailable`, `rate_limited`, `bad_frame`,
  `internal`). Whether it is fatal is implied by the code (auth/version errors
  close the socket; `peer_unavailable`/`rate_limited` are advisory).
- `bye { reason? }` — graceful shutdown notice before either side closes.

### 5.7 Machine name (desktop → relay → phone, v1 amendment)

The unified multi-Mac feed (§10) tags each item with the machine it came from.
The user-facing **default** name comes from the desktop and must auto-update when
the Mac is renamed, so the machine name travels on an **authenticated post-auth
frame that is re-sent on every connect** — never in the QR/code payload (which
would not auto-update).

- **Desktop → relay.** The desktop puts its name in
  `auth_response.machine_name` (§5.1). The relay length-bounds it (≤ **64
  characters**, trimmed; an empty result is treated as "no name") and stores it
  per pairing. It is **untrusted display text**: display-only, never used for
  routing or auth. Producers should still emit `"machine_name": null` when they
  have none (fixtures emit explicit `null`).
- **Relay → phone.** The relay delivers the name in a dedicated per-pairing
  frame so a phone holding several pairings can attribute each name correctly:

  ```json
  { "type": "machine_name", "pairing_id": "pair_ruud_mbp",
    "machine_name": "Ruud's MacBook Pro" }
  ```

  The relay emits it **to the phone**:
  1. right after the phone authenticates, once per activated pairing whose
     desktop has already announced a name, **and**
  2. whenever the desktop (re)connects and announces a name while the phone is
     online — so a **Mac rename propagates on the desktop's next connect**.

- **Phone behavior.** Treat the value as the pairing's new **default** name (a
  user override on the phone always wins) and **sanitize before display**. A
  pairing for which no name was ever announced simply never receives this frame;
  keep the previous/fallback name. The relay holds the name only for the live
  pairing (an in-memory relay loses it on restart; the desktop repopulates it on
  its next connect), so no persistence beyond the pairing is required.

### 5.8 Unpair / revoke (phone → relay → desktop, v1 amendment)

Unpairing one machine from the phone must both delete the phone's local pairing
record **and** revoke the pairing on the relay so the desktop learns it is
unpaired — without disturbing the phone's other pairings.

```
phone   → relay : revoke { pairing_id }
relay   → desktop: pairing_revoked { pairing_id }   // the former peer, if connected
relay   → phone : pairing_revoked { pairing_id }    // idempotent success confirmation
```

- **Membership check (mandatory security invariant).** The relay **must** verify
  the authenticated `device_id` is a member of `pairing_id` before removing
  anything. A **non-member** revoke is refused with
  `error { code: unknown_pairing, pairing_id }` and changes nothing. (Revocation
  is role-agnostic — any member may revoke — but v1 only the phone initiates.)
- **What removal clears.** On success the relay atomically deletes the pairing
  and **all** of its state: membership, any live claim tokens, both
  `(pairing, sender)` envelope queues, the push token, and the live routing-table
  entry. Then it sends `pairing_revoked` to the former peer (so a desktop can
  drop the pairing and return to an unpaired, **re-pairable** state) and back to
  the revoker as confirmation.
- **Idempotent.** Revoking a pairing that is already gone is a **success no-op**:
  the relay still replies `pairing_revoked { pairing_id }` and never errors.
- **Scope.** Only the named pairing is affected; the device's other pairings are
  untouched. The desktop, on `pairing_revoked`, drops that pairing's local record
  and tears down its E2E channel.

---

## 6. Queued delivery, sequencing, resume & dedup

This is the heart of "never lose an event, never send blind."

### 6.1 The envelope & sequence numbers

Application payloads are carried by `envelope` frames wrapping an
`EncryptedEnvelope`:

```json
{ "type": "envelope", "pairing_id": "…", "seq": 42, "sender": "desktop",
  "sent_at_ms": 1752412802000, "nonce": "…", "ciphertext": "…" }
```

- `seq` is a **monotonic, gapless counter per (pairing_id, sender)**, starting at
  **1**. The desktop's outbound stream and the phone's outbound stream each have
  their own independent sequence for a given pairing.
- The sender assigns `seq`. The relay preserves order and **queues** envelopes
  for a peer that is currently disconnected (bounded by "the Mac must be
  running" — PRD §9).
- **Seq-gap enforcement (v1 amendment).** The relay rejects an inbound envelope
  whose `seq` is not exactly `high_water + 1` for its (pairing, sender) — a
  regression or a gap is a `bad_frame` error and the envelope is not queued. A
  duplicate re-send of the current high-water `seq` is tolerated as an idempotent
  no-op (reconnect races), not an error. This makes the queue's ordering
  guarantee exact rather than best-effort.
- **Queue bound & overflow (v1 amendment).** Each (pairing, sender) buffer is
  bounded to `QUEUE_MAX_PER_PAIRING` un-acked envelopes (default 1000). On
  overflow the relay drops the **oldest** buffered envelope to make room for the
  newest and emits `error { code: rate_limited, pairing_id }` to the sender as an
  advisory back-pressure signal (the drop is silent to the offline peer; the
  sender learns its backlog is being shed). Correctness still relies on the
  desktop persisting its own outbound stream — a dropped-then-needed envelope is
  recovered by the sender re-queuing, not by the relay. (The PRD did not specify
  overflow behavior; drop-oldest is chosen so the newest state always wins.)

### 6.2 Acks

`ack { pairing_id, cursor }` acknowledges **contiguous** receipt of the peer's
envelopes up to and including `cursor` (the highest in-order `seq` the acker has
**durably** handled). The relay MAY discard queued envelopes with `seq ≤ cursor`.
Acks are cumulative; sending `ack cursor=41` implicitly acks 1..=41.

### 6.3 Resume-from-cursor

After (re)connecting and authenticating, a client sends
`resume { pairing_id, from_seq }` where `from_seq` is the **highest incoming
`seq` it already holds** for that pairing. The relay replays every queued
envelope with `seq > from_seq`, in order, then resumes live delivery. A fresh
install / first connect sends `from_seq: 0`.

### 6.4 Dedup / idempotency (normative)

Redelivery is expected (reconnect races, at-least-once relay). Receivers must be
idempotent:

- **Envelope dedup:** a receiver tracks the highest processed `seq` per
  (pairing, sender) and **ignores any envelope whose `seq` is ≤ that**. Ordered,
  gapless `seq` makes this exact, not best-effort.
- **Command idempotency:** every `PhoneCommand` carries a client-generated
  `command_id`. The desktop keeps a record of processed command IDs. Re-receiving
  a known `command_id` must be a **no-op that re-emits the original outcome** —
  and if the original result is no longer available, it replies
  `command_ack { outcome: duplicate }`. This makes phone retries safe: the phone
  may resend a command it has not yet seen acked without risking double-execution
  (e.g. it must never launch two agents or apply a git action twice).
- **Event dedup:** `AgentEvent.event_id` deduplicates the Activity feed and
  notifications, so a queued-then-replayed "needs input" event doesn't double-fire
  (PRD §5.8 "deduplicated").

### 6.5 Delivery honesty (the UI contract)

The phone shows a command as **"not delivered — retry"** until it receives a
`command_ack` for that `command_id` (PRD §5.8, §8). Because commands ride inside
E2E envelopes, "delivered" means the **desktop** acked at the application layer —
not merely that the relay accepted the envelope. On a lost link the phone pauses
new commands rather than sending blind.

---

## 7. The E2E model (what this crate does *not* do)

- The plaintext of an envelope is a **serialized E2E message**: a
  `DesktopToPhone` (desktop→phone) or a `PhoneCommand` (phone→desktop). Serialize
  to JSON, seal, and place the base64 result in `ciphertext`.
- **Sealing is out of the `flightdeck-remote-protocol` crate.** It lives in the
  desktop's `src/remote/crypto.rs` and the iOS `E2EChannel.swift`, proven
  byte-compatible by `remote/protocol/tests/fixtures/e2e_crypto/vectors.json`.
- The relay **cannot** read, and must not depend on, anything inside
  `ciphertext`. All routing decisions use `pairing_id` (and connection role).

### 7.1 E2E channel construction (normative, v1 — pinned)

> **⚠️ AMENDMENT (E2E task).** The original §7 left the sealing construction
> deliberately unpinned ("engineering detail, not pinned here"). It is now pinned
> exactly, because both platforms must derive identical keys and produce
> byte-identical ciphertext. Where this text and the fixtures disagree, the
> fixtures win.

1. **Input keying material (IKM).** A **static-static P-256 ECDH** between the two
   devices' **key-agreement (KA) keypairs** — the keys whose public points are
   exchanged during pairing as `key_agreement_public_key` (§5.2), **not**
   necessarily the identity keys used for relay auth (§5.1). Each endpoint feeds
   its own KA private key and the peer's KA public key (delivered in
   `pairing_claimed.peer_key_agreement_public_key`) into the ECDH. Both endpoints
   compute the identical shared secret; the IKM is its **big-endian
   x-coordinate**, 32 bytes (Rust `p256::ecdh` `SharedSecret::raw_secret_bytes`;
   CryptoKit `SharedSecret` raw bytes). This input exists on **both** pairing
   paths (QR and 4-digit code), since both exchange KA public keys.
   * *Why a distinct KA key.* On iOS the identity key is a Secure-Enclave signing
     key whose scalar cannot be applied to ECDH, so iOS uses a separate software
     P-256 KA key (§5.2). The **desktop MAY reuse** its identity key as its KA key
     (its keystore key is usable for both), in which case its
     `key_agreement_public_key` equals its `device_public_key`; the derivation is
     identical either way, since it operates on whichever KA keys were exchanged.
   * *Forward secrecy:* v1 has **none** — the long-lived KA keys are used
     directly. Ephemeral-key rotation is deferred (PRD §13). Documented, not an
     oversight.
2. **Salt = the `claim_token` bytes (reconciled contract, v1).** The HKDF salt is
   the UTF-8 bytes of the effective `claim_token`, on **both** the QR and the
   4-digit-code paths. This binds the derived keys to *this* pairing act (an
   attacker with a device key but no bootstrap observation still cannot derive the
   channel keys). The `claim_token` never transits the relay's E2E plane — it is
   the token the relay minted, known to both endpoints (the desktop displays it;
   the QR carries it; the 4-digit code *is* it).
   * *Why not the QR `pairing_secret`?* The desktop derives the channel from the
     `pairing_claimed` notification and **cannot know which path the phone used**,
     so a path-dependent salt (the 32-byte `pairing_secret` for QR vs. the token
     for the code) would be underivable on the desktop. The `claim_token` is the
     one value both endpoints share on both paths, so it is the only deterministic
     choice. The QR still carries a random `pairing_secret` field for wire
     compatibility with the iOS decoder, but it is **not** used in key derivation.
   * *Entropy trade-off.* With a 4-digit `claim_token` the salt is low-entropy.
     That is acceptable because the salt is only defence in depth: the channel's
     confidentiality rests on the static-static P-256 ECDH between the KA keys
     (whose private scalars never leave the devices and never transit the relay),
     not on the salt. Short TTL + single use + the per-connection claim rate limit
     (§5.2) bound the token's exposure window.
   * **iOS side (must match):** set the E2E salt to the `claim_token` UTF-8 bytes
     on **both** pairing paths (e.g. `PairingRecord.saltB64 =
     base64Standard(claimToken.utf8)`), regardless of whether the user scanned the
     QR or typed the code. Do **not** use the QR `pairing_secret` as the salt.
3. **KDF = HKDF-SHA256(ikm, salt)**, expanded twice into two independent 32-byte
   AEAD keys, one per direction:
   * `info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":d2p"` → **desktop→phone**
   * `info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":p2d"` → **phone→desktop**
     (all UTF-8; `pairing_id` is the id's string form).
4. **AEAD = ChaCha20-Poly1305** with a fresh **random 12-byte nonce** per message
   (the envelope's `nonce`, base64 standard-padded). The `ciphertext` field is the
   AEAD output **with the 16-byte Poly1305 tag appended** (CryptoKit's separate
   `ciphertext`/`tag` are concatenated in that order), base64 standard-padded.
5. **AAD (mandatory, not merely SHOULD).** The AAD is the **UTF-8 of the canonical
   string** `pairing_id ‖ ":" ‖ seq ‖ ":" ‖ sender ‖ ":" ‖ sent_at_ms`, where
   `seq`/`sent_at_ms` are base-10 integers and `sender` is `desktop`/`phone`. This
   binds the envelope header, so the relay cannot alter
   routing/ordering/attribution without the receiver's open failing. The receiver
   **must reject** any envelope whose header does not authenticate.

The desktop API is `E2eChannel::{derive, seal, open}` (plus a test-only
`seal_with_nonce`); iOS mirrors it as `E2EChannel`. The pairing-flow layer
derives one channel per pairing per endpoint and feeds `salt` (the QR
`pairing_secret` or the code's claim-token bytes) in at derive time.

---

## 8. E2E message taxonomy

### 8.1 Desktop → phone (`DesktopToPhone`, tag `type`)

| `type` | Payload | Purpose |
|--------|---------|---------|
| `snapshot` | `StateSnapshot` | Full state: `projects[] → sessions[]`, each session with name, `agent_type`, `status`, git indicators, running time, and a `pending_question` preview. Sent on connect and on `request_snapshot`. |
| `status_update` | `StatusUpdate` | Incremental `updates[]` of per-session `status` (+ optional running time / pending question). |
| `rollup` | `RollupUpdate` | Refreshed per-project `StatusRollup` (dot + plain-language summary + counts) without resending sessions. |
| `transcript` | `TranscriptFeed` | A cleaned transcript load for a session (`replace: true`). |
| `transcript_append` | `TranscriptFeed` | Incremental transcript items appended (`replace: false`). |
| `event` | `AgentEvent` | Typed event (`needs_input` / `finished` / `error`) with a `deep_link` payload. Drives pushes + Activity feed. |
| `git_status` | `GitStatusDetail` | Full git detail: branch, base, changed files, ahead/behind, drift. |
| `shell_output` | `ShellOutput` | A per-shell ordered chunk of stdout/stderr (may contain ANSI). |
| `shell_event` | `ShellEvent` | Shell lifecycle: `opened{cols,rows}` / `exited{code}` / `closed`. |
| `command_ack` | `CommandAck` | The desktop's ack of a phone command (delivery honesty). |

**Transcript items** (`TranscriptItem`, tag `type`): `user_message`,
`agent_message`, `activity` (collapsible pill: `summary`, optional `detail`,
optional expanded `body`, `kind`), `permission_prompt` (`command` text +
`options[]`). This directly models the design's cleaned transcript with activity
pills ("Edited auth.ts +18 −4", "Ran npm test · 42 passed") and inline
permission asks.

**Command outcomes** (`CommandOutcome`): `accepted` (validated, will apply),
`applied` (done), `rejected` (refused — e.g. failed type-to-confirm), `failed`
(attempted, errored — e.g. merge conflict), `duplicate` (idempotent no-op).

### 8.2 Phone → desktop (`PhoneCommand` = `command_id` + `issued_at_ms` + flattened `CommandBody`)

Every command is a `PhoneCommand`:

```json
{ "command_id": "cmd_00000001", "issued_at_ms": 1752412810000,
  "type": "reply", "session_id": "sess_fix_login", "text": "Yes, run it." }
```

| `type` | Key fields | Purpose |
|--------|-----------|---------|
| `reply` | `session_id`, `text` | Reply / follow-up prose to an agent. |
| `permission_decision` | `session_id`, `prompt_id`, `choice` | Resolve a permission prompt (`allow_once` / `deny`). |
| `new_agent` | `project_id`, `agent_type`, `name`, `base_branch`, `first_task` | Launch a new session (v1 fields only; model/effort inherit desktop defaults). |
| `restart_agent` | `session_id` | Fresh process, same worktree/branch, transcript preserved. |
| `close_session` | `session_id` | Close a session. |
| `set_manual_status` | `session_id`, `label` | Set the cyan manual override. |
| `clear_manual_status` | `session_id` | Clear the manual override. |
| `git_pull_base` | `session_id` | Pull base into the worktree (guarded). |
| `git_merge_back` | `session_id` | Merge branch back into base (guarded). |
| `git_abandon_worktree` | `session_id`, `confirm_name` | Destructive. Desktop MUST reject unless `confirm_name` equals the session name exactly (type-to-confirm, PRD §8). |
| `shell_open` | `session_id`, `shell_id`, `cols`, `rows` | Open the session's single shell. |
| `shell_input` | `session_id`, `shell_id`, `data` | Send input/keystrokes. |
| `shell_interrupt` | `session_id`, `shell_id` | Ctrl-C the foreground process. |
| `shell_close` | `session_id`, `shell_id` | Close the shell. |
| `request_snapshot` | `project_id?` | Ask for a fresh snapshot (all or one project). |
| `request_transcript` | `session_id`, `from_index?` | Ask for a transcript slice (or the whole thing). |
| `mark_read` | `event_ids[]` | Mark Activity events read. |

**Safety invariants preserved (PRD §8):** there is **no push / no PR / no
GitHub** command — pushing is the agent's job, not the remote's. The only
truly destructive command (`git_abandon_worktree`) carries a typed-confirmation
echo the desktop validates. Reads (`snapshot`, `git_status`, `transcript`) are
frictionless; state-changing commands are explicit and acked.

---

## 9. Status model

`AgentStatus` (tag `state`) matches FlightDeck's four states exactly:

- `working` — red spinner, actively running a turn.
- `idle` — green, turn done / ready.
- `needs_input` — orange, stopped and asking the human (most urgent).
- `manual { label }` — cyan user override with a label; clears on the next real
  state change (enforced desktop-side).

`StatusRollup` carries both the machine `dot` (`RollupDot`, precedence
**needs_input > working > manual > idle**) and the human `summary` string, plus
per-state counts, so the phone can render the project row without re-deriving
precedence.

---

## 10. Multi-Mac future (explicit)

Routing is **per-pairing** everywhere: a `pairing_id` identifies exactly **one
phone ↔ one Mac** pair. This is the extensibility hinge:

- One phone may later hold **many** pairings (one per Mac). It activates them all
  on a single connection via the `pairing_ids` list in `auth_response`, and
  addresses each independently — every envelope, ack, resume, presence and
  push-token frame is already scoped by `pairing_id`.
- Therefore **multi-Mac is almost entirely a UI addition** (PRD §9.1, Round 3):
  the wire format was already multi-pairing. The multi-pairing epic adds only two
  small relay-plane amendments so the phone can label and manage each Mac —
  the per-pairing **machine name** (§5.7) and **phone-initiated revoke** (§5.8);
  both are backward compatible and scoped by `pairing_id`.
- `[TBD]` multiple *phones* per Mac (PRD §13) — the model does not preclude it
  (each phone is a distinct device/pairing), but v1 assumes one active phone.

---

## 11. Worked end-to-end example (needs-input → reply)

1. Agent stops for a permission prompt. Desktop seals a `DesktopToPhone::Event`
   (`needs_input`, with `deep_link`) and sends `envelope { seq: N, sender: desktop }`.
   If the phone is offline, the relay queues it and (via the push service) fires
   an APNs notification using the registered token.
2. Phone wakes, connects, auths, sends `resume { from_seq: N-1 }`. The relay
   replays the queued envelope. Phone decrypts, dedupes by `event_id`, deep-links
   the user to the agent, and sends `ack { cursor: N }`.
3. User taps **Deny**. Phone seals a `PhoneCommand`
   (`permission_decision`, `choice: deny`, fresh `command_id`) and sends
   `envelope { seq: M, sender: phone }`. UI marks it "sending".
4. Desktop applies the decision idempotently, then seals
   `DesktopToPhone::CommandAck { command_id, outcome: applied }` +
   `transcript_append`. Phone clears the "sending" state on the ack; if the link
   had dropped, the command would show **"not delivered — retry"** instead.

---

## 12. Implementation notes per team

**Relay team:**
- Never inspect `ciphertext`. Route by `pairing_id` + role. Enforce that a
  connection only touches pairings it authenticated for.
- Persist per-pairing queues and sequence high-water marks so `resume` works
  across relay restarts; honor cumulative `ack` to trim queues.
- Treat delivery as **at-least-once**; correctness relies on receiver dedup (§6.4).
- Keep pairing-claim tokens short-TTL and single-use; rate-limit `pairing_claim`
  and `auth_response`.

**Desktop-bridge team:**
- Assign gapless `seq` per pairing for outbound envelopes; persist the last `seq`
  so it survives restarts (a reset would break phone dedup).
- Maintain a processed-`command_id` set for idempotency; re-emit prior outcomes
  (or `duplicate`) on repeats. Validate `git_abandon_worktree.confirm_name`.
- Emit `command_ack` for **every** command, always — the phone's honesty UI
  depends on it.

**iOS team:**
- Mirror every type in Swift `Codable` with these exact `snake_case`
  keys/tags. Verify against `tests/fixtures/` in a Swift test (decode → re-encode
  → compare) — those files are the contract.
- Emit explicit `null` for optionals (match the fixtures). Persist incoming
  high-water `seq` per (pairing, sender) and outbound `command_id`s so
  resume/retry are safe. Never send a command while `peer_presence` shows the
  desktop down.

---

## 13. File map

- `remote/protocol/src/ids.rs` — id newtypes.
- `remote/protocol/src/common.rs` — version constants, `Role`, `AgentType`,
  `AgentStatus`, `RollupDot`, git types, `negotiate_version` lives in `lib.rs`.
- `remote/protocol/src/relay.rs` — relay plane (`RelayFrame`, `EncryptedEnvelope`,
  presence/error/push enums).
- `remote/protocol/src/e2e.rs` — E2E plane (`DesktopToPhone`, `PhoneCommand`,
  transcript/event/shell/status types).
- `remote/protocol/tests/round_trip.rs` — fixture walker + invariant tests.
- `remote/protocol/tests/fixtures/{relay,desktop_to_phone,phone_to_desktop}/*.json`
  — one golden fixture per message variant (23 + 10 + 17 = 50). The relay set
  includes `machine_name`, `revoke`, and `pairing_revoked` (§5.7–§5.8).
