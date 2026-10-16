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
>
> **v4 amendments (epic remote-control-rjrz)** — desktop-decided, end-to-end
> encrypted visible pushes (§5.5, §7.2), relay modes (§5.9) and the direct
> phone ⇄ Mac link over Tailscale (§5.10) — are marked **v4** where they appear.

---

## 1. Design goals & the two-plane model

The protocol is split into two cleanly separated planes that share one WebSocket:

| Plane | Who reads it | Content | Defined in |
|-------|--------------|---------|------------|
| **Relay plane** | The relay **and** the peer | Plaintext, content-free metadata: versioning, auth, pairing, presence, delivery/sequencing, acks, latency, push tokens, push requests, errors | `relay::RelayFrame` |
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
  phone connects when foregrounded (e.g. after the user taps a push).
- **Other transports (v4).** Two more carry the same JSON types: the relay's
  HTTPS push endpoint `POST /v1/push` (§5.5.3), and the **direct link**, a
  WebSocket the Mac serves to a phone over Tailscale for direct-mode pairings
  (§5.10).
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
   **Exception — fields added to an existing frame after v1** are **omitted**
   when absent (`skip_serializing_if = "Option::is_none"`), so an older peer
   never sees an unfamiliar key: `hello.relay_password`, `error.expected_seq`,
   and the v4 fields `hello_ok.relay_mode`, `pairing_offer.direct_endpoint`,
   `pairing_claimed.direct_endpoint` and `pairing_claimed.peer_device_public_key`.
   Consumers must accept them both missing and `null`. Brand-new types (e.g.
   `PushPayload`) follow the explicit-`null` rule.
6. **Sequence/counter fields are unsigned** (`seq`, `cursor`, `from_seq`,
   `from_index` are `u64`; counts are `u32`).
7. **Newtype enum variants flatten.** `RelayFrame::Envelope(EncryptedEnvelope)`
   and the `DesktopToPhone` payload variants are newtype variants whose inner
   struct fields appear **flattened next to `type`**, not nested under a key.
   Likewise `PhoneCommand` flattens its `CommandBody`.

---

## 4. Versioning & negotiation

Every connection begins with version negotiation on the relay plane.

- Constants (this build): `PROTOCOL_VERSION = 4`, `MIN_SUPPORTED_VERSION = 1`,
  `MAX_SUPPORTED_VERSION = 4`. Every addition so far is additive, so v1 JSON
  still deserializes and `MIN` stays 1:
  - **v2** — multi-option prompts (§8.1/§8.2).
  - **v3** — multi-select (checklist) questions and multi-question forms.
  - **v4** — the `push_notify` frame (§5.5) and the optional fields
    `hello_ok.relay_mode` (§5.9), `pairing_offer.direct_endpoint`,
    `pairing_claimed.direct_endpoint` and `pairing_claimed.peer_device_public_key`
    (§5.2, §5.10). `push_notify` is a frame a v3 relay cannot parse, which is
    why v4 is a real bump: a desktop writes the `push_notify` frame only on a
    connection negotiated at **`PUSH_NOTIFY_MIN_VERSION` (4)** or above. Against
    an older relay it still tries the HTTPS endpoint (§5.5.3); that relay has
    none and answers 404, which the desktop logs and drops — it never retries,
    spins, or falls back to the frame. So a v3 relay gets **no** v4 pushes, and
    a v4 relay no longer sends the legacy wake pushes either: deploy the relay
    first, then desktops, then the phone app (§5.5 *Rollout order*).
    Likewise the relay puts a pairing in direct mode only for a phone that
    negotiated **`DIRECT_LINK_MIN_VERSION` (4)** or above (§5.2).
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
  if it is **lower than `local_min`**, it is incompatible. In this build only a
  preferred version **below 1** yields `version_incompatible`; a client
  preferring v5+ gets clamped to v4.
- The negotiated version governs the whole connection, including how the E2E
  payloads inside envelopes are interpreted. The E2E application messages do not
  carry a second version field in v1 — the relay-plane negotiated version is
  authoritative. (A future major E2E change would bump `PROTOCOL_VERSION`.)

---

## 5. Connection lifecycle & authentication

### 5.1 Normal (already-paired) connect

```
client → relay : hello { protocol_version, role, device_id, client }
relay  → client: hello_ok { protocol_version, server_time_ms, connection_id, relay_mode? }
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
- On failure the relay sends `error { code: auth_failed, … }` and closes. It
  rejects an `auth_response` only for an **unknown device** or a **bad
  signature**; pairing ids the device is not (or no longer) a member of are
  simply left out of `auth_ok`.
- **A relay that forgot the desktop (desktop behaviour).** A desktop whose
  auth-first reconnect is rejected `auth_failed` several times in a row
  (3 in this build) concludes the relay lost its record of the device (a wiped
  store). It drops its relay-mode pairings (they cannot work any more) and
  reports them for re-pairing. Its direct-mode pairings are kept — their data
  plane is the Mac's own link (§5.10) — but the relay can no longer push for
  them, and authenticating again would only be rejected again (the relay
  rejects the *device*), so the desktop: tells the user pushes are unavailable
  until the phone is re-paired; leaves those relay-unknown ids out of every
  later `auth_response`; **stops reconnecting**; and, when the user asks to
  pair, reconnects and runs the offer-first bootstrap (§5.2) — the pre-auth
  `pairing_offer` is the only frame that re-registers a device — before it
  authenticates with the new pairing id. It never loops on a rejection.
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
desktop → relay : pairing_offer { device_id, device_public_key, key_agreement_public_key, role, claim_token_hint, direct_endpoint? }
relay   → desktop: pairing_offer_ok { pairing_id, claim_token, expires_at_ms }
desktop → relay : auth_response { … }              // then proceeds as §5.1
…
// Phone side — redeem it (typically moments later, code entered / QR scanned):
phone → relay : hello { … role: phone … }
relay → phone : hello_ok { … }
relay → phone : auth_challenge { nonce, server_time_ms }
phone → relay : pairing_claim { claim_token, device_id, device_public_key, key_agreement_public_key, role }
relay → phone : pairing_claimed { pairing_id, peer_device_id, peer_key_agreement_public_key, direct_endpoint? }
              // (peer_key_agreement_public_key = the desktop's KA key;
              //  direct_endpoint = copied from the offer when the phone
              //  negotiated ≥ DIRECT_LINK_MIN_VERSION, v4)
              // (or error { code: pairing_claim_rejected } if token bad/expired)
relay → desktop: pairing_claimed { pairing_id, peer_device_id, peer_key_agreement_public_key, direct_endpoint?, peer_device_public_key? }
              // notifies the waiting desktop connection that the phone has
              // joined; peer_key_agreement_public_key = the phone's KA key;
              // direct_endpoint = echoed exactly when the phone got it (v4);
              // peer_device_public_key = the phone's identity key (v4)
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
- **Direct endpoint & direct mode (v4).** A desktop that serves the direct link
  (§5.10) puts its address in `pairing_offer.direct_endpoint`
  (`{ "host": "ruuds-mbp.tail1234.ts.net", "port": 47800 }` — the Mac's
  Tailscale MagicDNS name or `100.x` address, and the port its direct-link
  WebSocket listens on). The relay stores it with the pending pairing; it never
  dials it or routes by it.
  - **Direct mode needs a direct-capable phone.** Only a phone whose
    connection negotiated at least **`DIRECT_LINK_MIN_VERSION` (4)** dials the
    direct link (a v4 iOS build is the one that implements it). So the relay
    copies the endpoint verbatim into the **phone-bound**
    `pairing_claimed.direct_endpoint` **and echoes the same value in the
    desktop-bound** `pairing_claimed.direct_endpoint` if and only if the offer
    carried one **and** the claiming phone negotiated ≥ 4. Otherwise both
    copies omit it — an older phone would ignore the endpoint and keep using the
    relay, and the desktop must not wait for it on the direct link.
  - A pairing whose claims carried a `direct_endpoint` is in **direct mode on
    both ends**: the phone knows from its `pairing_claimed`, the desktop from the
    echo in its own (not from its offer — the offer alone says nothing about the
    phone). Every other pairing — including every pairing made before v4, and a
    direct offer claimed by a pre-v4 phone — is in **relay mode** and behaves
    exactly as before. Switching an existing pairing to direct mode means
    re-pairing; so does changing the endpoint.
  - **Push-only relays refuse claims that would be relay mode** (§5.9). On a
    `push_only` relay a claim whose pairing would end up in relay mode — the
    offer carried no `direct_endpoint`, or the claiming phone negotiated below
    `DIRECT_LINK_MIN_VERSION` so the endpoint would be withheld — is refused
    with `error { code: pairing_claim_rejected }` and a message saying what to
    do (`"this relay is push-only; update the iPhone app to pair over
    Tailscale"`, or `"…turn on the Mac's direct link (Tailscale) and pair
    again"`). The waiting desktop, if connected, receives the same error
    **carrying the `pairing_id`** of its offer and fails its pairing attempt
    with that message; the half-made pairing is revoked in the store. The token
    is spent (redemption is single-use). Defensively, a desktop that receives a
    `pairing_claimed` without the endpoint echo from a relay that announced
    `push_only` does not establish it: it drops the pairing, sends `revoke`, and
    reports the refusal. Offers themselves are not policed: a push-only relay
    still accepts an offer without an endpoint (an older desktop's).
- **Phone identity key to the desktop (v4).** The **desktop-bound**
  `pairing_claimed` additionally carries `peer_device_public_key`: the phone's
  identity public key, exactly the `device_public_key` from its
  `pairing_claim` (same base64 X9.63 encoding). The desktop stores it so it can
  authenticate the phone on the direct link itself (§5.10). The relay fills it
  on every v4 claim, direct mode or not (the key is public and cheap to store).
- **Which copy carries what.** One `pairing_claimed` type serves both
  directions; each side's copy fills only its own fields:

  | field | phone-bound copy (answer to `pairing_claim`) | desktop-bound copy (notification) |
  |---|---|---|
  | `peer_device_id` | the desktop's id | the phone's id |
  | `peer_key_agreement_public_key` | the desktop's KA key | the phone's KA key |
  | `direct_endpoint` (v4) | copied from the offer when the phone negotiated ≥ 4; omitted otherwise | the same value, echoed exactly when the phone got it; omitted otherwise |
  | `peer_device_public_key` (v4) | always omitted | the phone's identity key |

  Fixtures: `relay/pairing_offer_direct.json`, `relay/pairing_claimed_direct.json`
  (phone-bound), `relay/pairing_claimed_to_desktop.json` (desktop-bound, relay
  mode) and `relay/pairing_claimed_to_desktop_direct.json` (desktop-bound,
  direct mode: the echo); the v2-shaped `pairing_offer.json`/`pairing_claimed.json`
  still parse.
- **Pairing bootstrap always runs through the relay,** in both relay modes and
  for both pairing modes (§5.9 records why).
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

### 5.5 Push-token registration & push notifications

`register_push_token { pairing_id, token, environment }` → `push_token_ack`.
The APNs token is **opaque and outside E2E** — the relay holds it so it can
deliver the pushes the **desktop** requests with `push_notify` (§5.5.1) for that
pairing (PRD §9.1). `environment` is `sandbox` | `production`. A direct-mode
pairing (§5.10) registers its token with the relay exactly the same way.

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

**The desktop decides pushes; the relay sends no pushes of its own (v4).** Before
v4 the relay sent a silent `content-available` wake push for every envelope it
queued for an offline phone, and the phone had to reconnect, resume, decrypt and
post a local notification inside its background window. iOS throttles and drops
silent pushes, and one wake per envelope was noisy. From v4:

- **Queued envelopes trigger no push.** The relay never pushes on its own
  initiative — not on enqueue, not on presence changes. An offline phone catches
  up on its next open/reconnect (§6.3).
- **The desktop alone decides** when something is push-worthy — an agent needs
  input (permission prompt or question), hit an error, or is **done** (settled
  idle after a desktop-side debounce, never a mid-turn flicker) — and asks the
  relay for exactly one visible alert with `push_notify`.
- **The alert is visible and carries an E2E-encrypted payload** (§7.2) that the
  phone's Notification Service Extension decrypts to replace the generic text.
  The relay stays zero-knowledge: it sees only the pairing, the event id and the
  `class`.
- The same path serves both relay modes (§5.9) and both pairing modes (§5.10).
- **Rollout order: relay, then desktops, then the phone app.** A v4 relay no
  longer sends wake pushes, so a v3 desktop paired through it gets no pushes at
  all until it is updated; a v3 relay has no `push_notify` (a v4 desktop's
  HTTPS push gets 404 and is dropped, §4); and a v4 phone app no longer acts on
  silent wake pushes, so it should arrive last, once pushes come as v4 alerts.

#### 5.5.1 `push_notify` (desktop → relay, v4)

```json
{ "type": "push_notify", "pairing_id": "pair_ruud_mbp",
  "event_id": "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b", "class": "needs_input",
  "nonce": "AAECAwQFBgcICQoL", "ciphertext": "AP5g0H3C…" }
```

- **Fields** (`relay::PushNotifyBody`, flattened next to `type` like
  `envelope`): `pairing_id`; `event_id` (the `AgentEvent.event_id` this push
  announces, a UUID string ≤ 64 bytes); `class` (`PushClass`: `needs_input` |
  `error` | `done` — the **only** plaintext hint); `nonce` and `ciphertext`
  (base64 standard-padded; the sealed `PushPayload`, §7.2).
- **Who may send it.** Only an authenticated **desktop** that is a member of
  `pairing_id`. A non-member desktop (or an unknown pairing) gets
  `error { code: unknown_pairing, pairing_id }`; a **phone** sending it gets
  `error { code: bad_frame }`. Before `auth_ok` it is `not_authenticated`, as for
  any post-auth frame.
- **Size.** A `ciphertext` longer than **3000 characters**
  (`MAX_PUSH_CIPHERTEXT_B64_LEN`) is refused with `error { code: bad_frame,
  pairing_id }`.
- **Fire-and-forget.** There is no success reply on the WebSocket; failures
  surface as `error` frames. If the pairing has **no registered push token**, the
  relay silently does nothing (debug log, no content).
- **Rate limit.** The relay limits accepted pushes **per desktop device**
  (not per pairing), counting the WebSocket frame and `POST /v1/push` against
  one budget: 60 per fixed one-minute window in this build
  (`DEFAULT_PUSH_RATE_LIMIT_PER_MINUTE`). Over budget it answers
  `error { code: rate_limited, pairing_id }` on the WebSocket (advisory: the
  desktop logs it and does not treat it as a queue overflow, §6.1) or `429
  rate_limited` over HTTPS. The limit applies after membership and size
  validation, so refused pushes do not spend it.
- **Desktop transport rule.** Send `push_notify` over the relay WebSocket when it
  is connected **and authenticated** at a negotiated version ≥ 4 **and has
  delivered a relay frame within the last 25 s** (`PUSH_WS_MAX_SILENCE`, just
  over the 20 s ping interval — a quieter socket may be half-open, accepting
  writes that never arrive until the 60 s liveness teardown); otherwise POST the
  same fields over HTTPS (§5.5.3). The rule is applied again when the frame is
  about to be written: a push that waited more than 30 s
  (`PUSH_WS_MAX_QUEUE_AGE`), meets a session negotiated below v4, finds the
  socket silent, or whose write fails, is POSTed over HTTPS instead; and a push
  is **never held for a later session** — one queued while no session is
  authenticated is POSTed at once. The `event_id` is the same on every route,
  so the rare WS + HTTPS double delivery collapses into one alert on the phone
  (it is the APNs collapse id, §5.5.2). It is never sent on the direct link
  (§5.10) — the Mac is the server there and pushes always go via the relay.
- Fixture: `relay/push_notify.json` (the same push as case 0 of
  `e2e_crypto/push_vectors.json`).

#### 5.5.2 The APNs request the relay builds

For each accepted `push_notify` with a registered token, the relay sends one
APNs request to the token's environment:

- **Headers:** `apns-push-type: alert`, `apns-priority: 10`,
  `apns-collapse-id: <event_id>` (a retried push for the same event replaces the
  alert instead of duplicating it), `apns-expiration: <now + 24 h, unix
  seconds>` (non-zero, so APNs stores and forwards to an offline phone),
  `apns-topic: <bundle id>` (relay config).
- **Body:**

  ```json
  { "aps": { "alert": { "title": "FlightDeck", "body": "An agent needs your input" },
             "mutable-content": 1,
             "sound": "default",
             "interruption-level": "time-sensitive",
             "thread-id": "pair_ruud_mbp" },
    "fd_push": { "v": 1, "pairing_id": "pair_ruud_mbp",
                 "event_id": "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b",
                 "class": "needs_input",
                 "nonce": "AAECAwQFBgcICQoL", "ciphertext": "AP5g0H3C…" } }
  ```

  - `aps.alert.body` is **generic per class**, never content:
    `needs_input` → `"An agent needs your input"`, `error` → `"An agent hit an
    error"`, `done` → `"An agent is done"`. `aps.alert.title` is always
    `"FlightDeck"`.
  - `interruption-level` is `time-sensitive` for `needs_input` and `active` for
    `error` and `done`. `thread-id` is the `pairing_id` (groups one Mac's
    alerts).
  - `mutable-content: 1` lets the phone's Notification Service Extension open
    `fd_push` (§7.2) and replace the title/body. If the extension cannot open it
    (no pairing record, tampering, timeout), the generic alert is shown as-is.
  - `fd_push` copies the five `push_notify` fields verbatim plus `"v": 1` (the
    `fd_push` layout version). With the 3000-character ciphertext cap the whole
    payload stays under APNs' 4 KiB limit.
- **Logging.** One line per push attempt: `pairing_id`, `event_id`, `class`,
  outcome (`ok` / `transient` / `permanent` + token purged / `no token`). Never
  any content.
- The pre-v4 silent `background_wake` push and the plaintext
  `alert_for_event` path are gone.

#### 5.5.3 HTTPS push endpoint `POST /v1/push` (v4)

The same request over HTTPS, for a desktop with no live, authenticated relay
WebSocket (always available; the only desktop path to a push-only relay when it
holds no WebSocket). The desktop derives the URL from its configured relay URL:
`wss://host[:port]/…` → `https://host[:port]/v1/push`, `ws://…` → `http://…`
(`relay::push_endpoint_url`).

```json
{ "device_id": "dev_mac_7b21", "timestamp_ms": 1752412800500,
  "push": { "pairing_id": "pair_ruud_mbp", "event_id": "6f9a1c2e-…",
            "class": "needs_input", "nonce": "AAECAwQFBgcICQoL",
            "ciphertext": "AP5g0H3C…" },
  "signature": "<base64 raw r‖s, 64 bytes>" }
```

- **Body** (`relay::PushRequest`, `Content-Type: application/json`): the
  requesting `device_id`, the desktop's clock `timestamp_ms`, the five
  `push_notify` fields nested under `push` (`PushNotifyBody`), and a
  `signature`.
- **Signature.** ECDSA P-256 / SHA-256 by the device identity key (same key and
  encodings as §5.1), raw `r ‖ s`, base64, over the UTF-8 bytes of this
  canonical message — `\n`-joined, **no trailing newline**, `timestamp_ms`
  base-10, `class` its wire value (`relay::push_request_signing_message`):

  ```text
  flightdeck-remote-push-v1
  {device_id}
  {timestamp_ms}
  {pairing_id}
  {event_id}
  {class}
  {nonce}
  {ciphertext}
  ```

- **Relay checks.** The relay password, if configured, in header
  `x-flightdeck-relay-password` (constant-time compare, as for
  `hello.relay_password`); the signature against the device's **registered**
  identity key; `|relay_now − timestamp_ms| ≤ 300 000` ms; the device's role is
  **desktop**; the device is a member of `push.pairing_id`. Then it proceeds
  exactly as for the WebSocket frame (§5.5.1, §5.5.2), including the silent
  no-op when no token is registered.
- **Responses.**

  | Status | `code` | When |
  |---|---|---|
  | 202 Accepted | — | Accepted (including "no token registered"). Empty body; clients ignore any body. |
  | 400 | `bad_frame` | Malformed JSON or fields; the device is not a desktop (mirrors the WS rule). |
  | 401 | `auth_failed` | Missing/wrong relay password, unknown device, bad signature, or timestamp outside ±300 s. |
  | 404 | `unknown_pairing` | The desktop is not a member of `push.pairing_id` (or it does not exist). |
  | 413 | `bad_frame` | `ciphertext` over 3000 characters, or an oversized request body. |
  | 429 | `rate_limited` | Rate limit hit. |
  | 500 | `internal` | Relay-side failure. |

  Every non-2xx response has the JSON body `{ "code": "<RelayErrorCode>",
  "message": "…" }` (`relay::PushErrorBody`); `message` never contains push
  content.
- Fixtures: `push_http/push_request.json`, `push_http/push_error.json`.

### 5.6 Errors & shutdown

- `error { code, message, pairing_id?, expected_seq? }` — machine-readable
  `RelayErrorCode` (`unsupported_version`, `auth_failed`, `unknown_pairing`,
  `not_authenticated`, `pairing_claim_rejected`, `peer_unavailable`,
  `rate_limited`, `bad_frame`, `seq_violation` (§6.1), `relay_mode_push_only`
  (v4, §5.9), `internal`). Whether it is fatal is implied by the code
  (auth/version errors close the socket; `peer_unavailable`/`rate_limited`/
  `seq_violation`/`relay_mode_push_only`/`pairing_claim_rejected`/
  `unknown_pairing` are advisory).
- **`bad_frame` carrying a `pairing_id` is advisory.** The relay closes the
  socket after every connection-level `bad_frame` (a malformed frame, a wrong
  first frame, an invalid offer or claim), and those never name a pairing. A
  `bad_frame` that names a `pairing_id` refuses one frame for that pairing (an
  oversized or malformed `push_notify`, §5.5.1; an envelope whose sender does
  not match) and the connection stays open, so a client MUST NOT reconnect on
  it — reconnecting would only meet the same refusal. Fatal codes for a client:
  `auth_failed`, `unsupported_version`, `not_authenticated`, `internal`, and
  `bad_frame` without a `pairing_id`; everything else is advisory.
- **`pairing_claim_rejected` to a desktop** carries the `pairing_id` of the
  desktop's pending offer when a phone's claim of it was refused (§5.2,
  push-only relays); the desktop forgets that offer and shows the message.
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

### 5.9 Relay modes (v4)

One relay binary runs in one of two modes, chosen by its `RELAY_MODE`
configuration (`full` | `push_only`, default `full`), and announces it in
`hello_ok.relay_mode` (`RelayMode`). A pre-v4 relay omits the field; treat
absent as `full`.

| | `full` | `push_only` |
|---|---|---|
| `hello` / auth / `version_incompatible` | ✓ | ✓ |
| `pairing_offer` / `pairing_claim` (bootstrap) | ✓ | ✓ |
| `machine_name`, `revoke` / `pairing_revoked` | ✓ | ✓ |
| `register_push_token` / `unregister_push_token` | ✓ | ✓ |
| `ping` / `pong`, `peer_presence` | ✓ | ✓ |
| `push_notify` (WS) and `POST /v1/push` (HTTPS) | ✓ | ✓ |
| `envelope`, `resume`, `ack` (the data plane, §6) | ✓ | refused |

- **Refusal.** A push-only relay answers `envelope`, `resume` and `ack` with
  `error { code: relay_mode_push_only, pairing_id }` and keeps the connection
  open (fixture: `relay/error_relay_mode_push_only.json`). It runs no
  per-pairing sender queues and no seq tracking.
  - The refusal is checked **first**, after authentication: it does not depend
    on whether `pairing_id` is activated on the connection (the mode is the
    reason, so no `unknown_pairing` is ever returned for these frames), and
    `pairing_id` echoes the frame's. Before `auth_ok` these frames are still
    `not_authenticated` + close, as in any mode.
  - Nothing about a refused frame reaches the peer: no envelope is forwarded,
    no ack is relayed, nothing is queued.
  - The refusal repeats per frame (no escalation to a close); a client that
    keeps sending is misconfigured and SHOULD stop after the first one.
  - A full relay sends each freshly authenticated leg an unsolicited
    `ack { pairing_id, cursor }` per activated pairing (its own stream's ack
    position, which also tells the endpoint that this relay forwards acks). A
    push-only relay **does not**: it forwards no acks, so an endpoint must not
    arm an ack-based liveness deadline against it.
- **Configuration.** `RELAY_MODE` unset or empty means `full`; the value is
  trimmed and case-insensitive. Any other value is a startup error (the relay
  refuses to start rather than silently falling back to `full`). The relay
  logs its mode at startup and reports it on `GET /version` as `relay_mode`
  next to `version` and `git_sha`; the `/healthz` and `/readyz` probes are
  unchanged (plain `ok`).
- **Storage.** A push-only relay still persists device keys, pairings, claim
  tokens and push tokens (the `FLIGHTDECK_RELAY_STORE` backend works as in
  `full`); the envelope-queue tables are never written, so they stay empty.
  Switching an existing `full` relay to `push_only` leaves any already-queued
  envelopes untouched but unreachable (they replay again if it is switched
  back).
- **Who uses which.** `full` is today's routing relay, for adopters who want
  relay-mode pairings. `push_only` is for deployments where every pairing is in
  direct mode (§5.10), so the relay only needs to authenticate devices and send
  pushes. A desktop that sees `relay_mode: push_only` SHOULD only offer
  direct-mode pairings (a relay-mode pairing would have no data plane); the relay
  does not police offers, but it **refuses a claim** whose pairing would be relay
  mode (§5.2), so such a pairing is never minted. A desktop that cannot offer a
  direct endpoint tells the user why: the direct link is switched off, or it is
  on but not listening yet (Tailscale not running or not connected).
- **Decision: pairing bootstrap stays on the relay in both modes.** It would be
  possible to pair directly over Tailscale, but the relay keeps
  `pairing_offer`/`pairing_claim` because:
  1. the relay needs each device's identity key and the pairing membership
     anyway — to authenticate `push_notify` (WS and HTTPS), push-token
     registration and `revoke` — and pairing through it is how it learns them
     with proof (each side self-registers its key in its own frame);
  2. the claim token needs a rendezvous both devices can reach before either
     knows the other's address, and the phone learns the Mac's
     `direct_endpoint` from that rendezvous;
  3. one pairing flow for both pairing modes and both relay modes keeps the
     desktop, the phone and the fixtures single-path.

### 5.10 Direct link (phone ⇄ Mac over Tailscale, v4)

A **direct-mode** pairing (§5.2) carries its whole data plane over a WebSocket
the Mac serves, instead of through the relay. Tailscale (WireGuard) provides
transport encryption and reachability; the E2E envelopes (§7.1) run on top
unchanged.

- **Endpoint.** `ws://<host>:<port>/remote/v1` (`relay::DirectEndpoint::ws_url`,
  path `DIRECT_LINK_PATH`) from the pairing's `direct_endpoint`. The desktop
  serves it only when the user opts in, and binds **only** to its Tailscale
  interface address(es) (or loopback, for tests) — never `0.0.0.0` — mirroring
  web network mode. "Tailscale address" means one `tailscale status --json`
  reports for this node (`Self.TailscaleIPs`) whenever the CLI answers (none
  reported → nothing bound); only when the CLI cannot be asked does the desktop
  fall back to the address ranges `100.64.0.0/10` / `fd7a:115c:a1e0::/48` —
  the IPv4 range is shared with carrier-grade NAT, so it is never preferred.
- **Framing.** The **same `RelayFrame` JSON** as the relay, with the Mac in the
  relay's role:

  ```
  phone → mac : hello { protocol_version, role: phone, device_id, client }
  mac   → phone: hello_ok { protocol_version, server_time_ms, connection_id }  // relay_mode omitted
  mac   → phone: auth_challenge { nonce, server_time_ms }
  phone → mac : auth_response { device_id, signature, pairing_ids }
  mac   → phone: auth_ok { pairing_ids }
  mac   → phone: machine_name { pairing_id, machine_name }                    // per pairing
  mac   → phone: peer_presence { pairing_id, peer: desktop, state: connected, at_ms }
  phone → mac : resume { pairing_id, from_seq }
  mac   → phone: envelope { … }                                               // replayed, then live
  ```

  Then `envelope` both ways, `ack`, `ping`/`pong`, `revoke`/`pairing_revoked`,
  `error` and `bye` behave as on the relay. Version negotiation is the same
  (§4). `hello_ok.relay_mode` is **omitted** (the Mac is not a relay and carries
  the full data plane for its own pairings). Bootstrap and push frames
  (`pairing_offer`, `pairing_claim`, `register_push_token`,
  `unregister_push_token`, `push_notify`) are not served on the direct link
  (`error { code: bad_frame }`). `hello.relay_password` is ignored.
- **Auth.** The phone signs `b"flightdeck-remote-direct-v1:" ‖ <decoded nonce
  bytes>` (`relay::direct_auth_message`) with its identity key — ECDSA P-256 /
  SHA-256, raw `r ‖ s`, base64, as in §5.1. The domain prefix makes a
  direct-link signature useless as a relay `auth_response` (which signs the bare
  nonce) and vice versa. The Mac verifies it against the
  `peer_device_public_key` it stored from the desktop-bound `pairing_claimed`
  (§5.2). Every requested `pairing_id` must be a live direct-mode pairing whose
  peer is this `device_id`; an unknown or revoked device or pairing, or a bad
  signature, gets `error { code: auth_failed }` and the socket closes.
- **Desktop outbox.** For direct-mode pairings the desktop sends envelopes
  **only** to its direct outbox, never via the relay. The outbox keeps un-acked
  desktop→phone envelopes per pairing, bounded (newest 1000 envelopes / 8 MiB,
  oldest dropped first), **persisted** so a Mac restart keeps seq continuity;
  it replays strictly above `resume.from_seq` and drops envelopes at or below an
  `ack` cursor. Sequencing, AAD and gapless numbering are exactly §6.1. A
  `from_seq` that precedes the oldest retained envelope (overflow shed the gap)
  gets `error { code: seq_violation, pairing_id }`, and the phone recovers as in
  §6.3 (drop its cursor, resume from 0, request a snapshot). Having reported the
  gap, the outbox treats the shed seqs as given up (its ack point moves to just
  below the oldest retained envelope), so that recovery `resume { from_seq: 0 }`
  is answered with the retained tail rather than another `seq_violation`.
- **Phone → desktop envelopes** are deduped by `seq` (§6.4) exactly as for relay
  delivery.
- **What still uses the relay.** A direct-mode pairing still uses the relay for
  push-token registration (§5.5), `revoke` (the relay must drop membership and
  the push token, so the phone always revokes there; it MAY also send `revoke`
  on the direct link) and pushes (`push_notify`, §5.5.1). The phone never sends
  `envelope`/`resume`/`ack` for a direct-mode pairing to the relay.
- Pairings made before v4 are relay mode and behave exactly as before; re-pair
  to switch.

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
  whose `seq` is **greater** than `high_water + 1` for its (pairing, sender) — a
  forward gap means envelopes went missing, so it is answered with
  `error { code: seq_violation }` and not queued. A duplicate re-send of the
  current high-water `seq` is tolerated as an idempotent no-op (reconnect races),
  not an error. This makes the queue's ordering guarantee exact rather than
  best-effort.
- **The sender owns its cursor; the relay follows it (v1 amendment,
  remote-control-arg).** The relay's `high_water` is a *cache* of the sender's
  cursor, not an independent authority, so the two cases where they legitimately
  disagree are not errors:
  - **Unknown stream.** The first envelope on a (pairing, sender) the relay has
    no record of is accepted at whatever `seq` it carries, and `high_water`
    adopts it. A relay that came up with an empty store must not demand `seq 1`
    from endpoints that both still hold matching cursors from before — that
    rejects every envelope the sender will ever send.
  - **Rewind.** A `seq` *below* `high_water` means the sender lost its cursor
    (reinstall, restored backup) and restarted its stream. The relay abandons its
    own epoch for that direction — discarding the envelopes it still holds, which
    belong to a stream that no longer exists — adopts the restarted cursor, and
    accepts the envelope. `seq_violation` is **not** the right answer here: the
    sender has no cursor left to drop and nothing to move forward to, so it
    restarts at the same low `seq` and is rejected again, forever.

  Because the relay absorbs both, **an endpoint must never renumber an outbound
  stream it is successfully sending.** Rewinding `last_sent_seq` was the v1
  response to `seq_violation`; against a relay that *persists* `high_water` it
  livelocks (remote-control-arg).
- **`seq_violation` means "your INBOUND cursor is stale".** It is sent to a
  receiver whose view of a stream is no longer serviceable: its peer restarted
  the stream (above), or a drop-oldest overflow shed envelopes it still needs
  (§6.3). The receiver drops its `last_received_seq` for that pairing,
  re-`resume`s from 0, and requests a fresh snapshot. It never changes the
  receiver's own outbound sequencing. When a rewind resets a stream, the relay
  sends this advisory to the peer *before* forwarding the restarted envelope, so
  the peer's dedup does not discard the new epoch's seqs as duplicates; a peer
  that is offline or misses it is caught by the `resume` rule in §6.3.
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

The relay answers `error { code: seq_violation }` instead of a replay when the
resume cannot be served without stranding the receiver:

- `from_seq > 0` precedes the oldest envelope still buffered, because a
  drop-oldest overflow shed the ones in between. Replaying would hand the receiver a stream
  with a permanent hole its gapless enforcement stalls on.
- `from_seq` is **above** the stream's `high_water` on a stream the relay knows.
  The sender restarted underneath this receiver (§6.1 rewind) while it kept the
  old epoch's cursor, so every `seq` the new epoch emits would be deduped away as
  a duplicate. An empty replay here reads as "you are up to date" and strands the
  receiver silently — the failure mode in remote-control-arg.

Either way the receiver applies the §6.1 `seq_violation` recovery: drop
`last_received_seq`, re-`resume` from 0, request a fresh snapshot.

**`resume { from_seq: 0 }` is never answered with `seq_violation` for an overflow
gap.** It means "I hold nothing for this pairing" — a fresh install, or a
receiver that just dropped its cursor in response to the advisory. The seqs shed
by overflow are given up for good: the relay replays whatever it still retains
(e.g. 3, 4, 5 after 1 and 2 were dropped) and the receiver accepts the first
replayed `seq` as its new cursor base (§6.4 dedup is `seq > last_received_seq`,
with no gap check), relying on the fresh snapshot for state it never saw.
Answering `from_seq: 0` with another `seq_violation` would make the recovery
itself fail and loop forever (remote-control-uj2t). The sequence for one
overflowed pairing is therefore: `resume(k)` with `0 < k < oldest−1` →
`seq_violation` once; `resume(0)` → replay of the retained tail.

### 6.4 Dedup / idempotency (normative)

Redelivery is expected (reconnect races, at-least-once relay). Receivers must be
idempotent:

- **Envelope dedup:** a receiver tracks the highest processed `seq` per
  (pairing, sender) and **ignores any envelope whose `seq` is ≤ that**. Ordered,
  gapless `seq` makes this exact, not best-effort. **Exception — stream
  restart:** a `seq` of 1 while the receiver holds a cursor ≥ 1 is not a
  duplicate. A healthy stream is monotonic and never re-emits 1, so this can only
  be the peer having lost its cursor and restarted (§6.1 rewind, which the relay
  adopts). Deduping it discards the recovered stream silently and forever, so the
  receiver must accept it and reset its cursor to the new epoch.
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
  byte-compatible by `remote/protocol/tests/fixtures/e2e_crypto/vectors.json`
  (envelopes) and `e2e_crypto/push_vectors.json` (push payloads, §7.2). The
  crate does carry the canonical *strings* both sides must agree on
  (`e2e::push_aad`, `relay::push_request_signing_message`,
  `relay::direct_auth_message`), so no implementation re-spells them.
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

### 7.2 Push payload sealing (normative, v4)

A push (§5.5) carries an encrypted `PushPayload` that the phone's Notification
Service Extension (NSE) opens to show real text in place of the generic alert.
The NSE has no live channel, no envelope `seq` and ~30 s of runtime, so the
payload uses its own key and AAD, derivable from the stored pairing record
alone.

1. **Key.** HKDF-SHA256 with the **same IKM and salt as §7.1** (the KA keys'
   static-static ECDH x-coordinate; the `claim_token` UTF-8 bytes), expanded with
   `info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":push"` (UTF-8) → 32
   bytes. **Direction-independent:** the desktop seals, the phone opens, and both
   derive the identical key.
2. **AEAD.** ChaCha20-Poly1305 with a fresh random 12-byte nonce per push; the
   `ciphertext` is the AEAD output with the 16-byte tag appended; both base64
   standard-padded — identical to §7.1 step 4.
3. **AAD.** The UTF-8 of `"push:" ‖ pairing_id ‖ ":" ‖ event_id`
   (`e2e::push_aad`). This binds the payload to the push's plaintext routing
   fields: the relay cannot replay one event's payload as another event or
   another pairing.
4. **Plaintext.** The UTF-8 JSON of `e2e::PushPayload`:

   ```json
   { "event_id": "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b", "class": "needs_input",
     "title": "fd-api needs input", "body": "Allow `cargo test --locked`?",
     "deep_link": { "project_id": "proj_fd_api", "session_id": "sess_fix_login",
                    "item_id": "item_q1" },
     "project_name": "fd-api", "occurred_at_ms": 1752412800000 }
   ```

   `event_id` and `class` equal the push frame's; `title` is the alert title;
   `body` the question preview, error message or turn summary (may be `""`);
   `deep_link` where a tap lands; `project_name` is display-only and may be
   `null`. Optionals are explicit `null` (§3 rule 5). Fixtures:
   `push_payload/needs_input.json`, `push_payload/done.json`.
5. **Size.** The desktop truncates on a `char` boundary and appends `…` (U+2026,
   counted in the budget) so that `title` ≤ **120 bytes** and `body` ≤ **1000
   bytes** (`e2e::truncate_push_text`, `MAX_PUSH_TITLE_BYTES`,
   `MAX_PUSH_BODY_BYTES`); the serialized plaintext MUST be ≤ **2048 bytes**
   (`MAX_PUSH_PLAINTEXT_BYTES`), which keeps the base64 ciphertext under the
   relay's 3000-character cap (§5.5.1).
6. **Phone behavior.** The NSE looks up the pairing by `fd_push.pairing_id`,
   derives the push key, opens `fd_push.ciphertext` with the AAD above, and
   replaces the alert's title/body with the payload's (keeping the deep link). On
   any failure it delivers the generic alert unchanged. The phone dedupes pushes
   against the `AgentEvent` with the same `event_id` it later receives over the
   data plane (§6.4).

The desktop API is `E2eChannel::{seal_push_payload, seal_push, open_push}`
(plus test-only `seal_push_payload_with_nonce`/`seal_push_with_nonce`); the push
key is derived alongside the directional keys in `E2eChannel::derive`.
`push_vectors.json` holds fixed KA private keys, salt, pairing id, derived push
key, event id, AAD, nonce, plaintext and ciphertext, plus a tampered-AAD case
that must fail to open; it is generated and locked by a test in
`src/remote/crypto.rs`, and the iOS tests verify against the same file.

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
| `event` | `AgentEvent` | Typed event (`needs_input` / `finished` / `error`) with a `deep_link` payload. Drives the Activity feed; the desktop sends the matching push separately as `push_notify` (§5.5, v4), deduped by `event_id`. |
| `git_status` | `GitStatusDetail` | Full git detail: branch, base, changed files, ahead/behind, drift. |
| `shell_output` | `ShellOutput` | A per-shell ordered chunk of stdout/stderr (may contain ANSI). |
| `shell_event` | `ShellEvent` | Shell lifecycle: `opened{cols,rows}` / `exited{code}` / `closed`. |
| `command_ack` | `CommandAck` | The desktop's ack of a phone command (delivery honesty). |

**Transcript items** (`TranscriptItem`, tag `type`): `user_message`,
`agent_message`, `activity` (collapsible pill: `summary`, optional `detail`,
optional expanded `body`, `kind`), `permission_prompt` (`kind`, `command` text,
`options[]`, `allow_free_text`). This directly models the design's cleaned
transcript with activity pills ("Edited auth.ts +18 −4", "Ran npm test · 42
passed") and inline permission asks.

A `permission_prompt` carries a `kind` (v2): `permission` is the binary
allow/deny ask (options `[allow_once, deny]`); `question` is a multiple-choice
ask (Claude Code `AskUserQuestion`, OpenCode `question.asked`). Each
`PermissionOption` has a stable 0-based `index`, a `label`, an optional
`description`, and — for `permission` kind only — a `choice` (`allow_once` /
`deny`). `allow_free_text` is `true` when the phone may submit a typed answer
("Type your own answer"). Both `kind` and `allow_free_text` default (to
`permission` / `false`) when absent, so a v1 prompt still parses.

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
| `permission_decision` | `session_id`, `prompt_id`, optional `choice`, optional `option_index`, optional `free_text` | Resolve a prompt. Binary fast-path sends `choice` (`allow_once`/`deny`); a `question` answer sends `option_index` (0-based) or `free_text`. Exactly one of the three is set; absent fields are omitted (not `null`). |
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
   (`needs_input`, with `deep_link`) and sends `envelope { seq: N, sender: desktop }`
   (to the relay, or to its direct outbox for a direct-mode pairing). If the phone
   is offline the envelope is queued — **queuing triggers no push** (v4). The
   desktop separately decides this event is push-worthy: it seals a
   `PushPayload` under the push key (§7.2) and sends
   `push_notify { pairing_id, event_id, class: needs_input, nonce, ciphertext }`
   over its relay WebSocket (or `POST /v1/push` when it has none). The relay sends
   one visible APNs alert (generic text, `fd_push` attached, §5.5.2); the phone's
   Notification Service Extension opens `fd_push` and shows "fd-api needs input"
   with the question preview.
2. The user taps the alert. The phone opens at the deep link, connects, auths,
   and sends `resume { from_seq: N-1 }`. The relay (or the Mac, on the direct
   link) replays the queued envelope. Phone decrypts, dedupes by `event_id`, and
   sends `ack { cursor: N }`.
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
- Send pushes **only** for `push_notify` / `POST /v1/push` (§5.5); never on
  enqueue. Log one content-free line per push attempt. Honor `RELAY_MODE`
  (§5.9).

**Desktop-bridge team:**
- Assign gapless `seq` per pairing for outbound envelopes; persist the last `seq`
  so it survives restarts (a reset would break phone dedup).
- Maintain a processed-`command_id` set for idempotency; re-emit prior outcomes
  (or `duplicate`) on repeats. Validate `git_abandon_worktree.confirm_name`.
- Emit `command_ack` for **every** command, always — the phone's honesty UI
  depends on it.
- Decide pushes (needs input, error, done-after-debounce), seal them (§7.2) and
  send `push_notify` over the WS at negotiated version ≥ 4, else over HTTPS
  (§5.5.3). For direct-mode pairings, serve the direct link and keep the outbox
  (§5.10).

**iOS team:**
- Mirror every type in Swift `Codable` with these exact `snake_case`
  keys/tags. Verify against `tests/fixtures/` in a Swift test (decode → re-encode
  → compare) — those files are the contract.
- Emit explicit `null` for optionals (match the fixtures). Persist incoming
  high-water `seq` per (pairing, sender) and outbound `command_id`s so
  resume/retry are safe. Never send a command while `peer_presence` shows the
  desktop down.
- Open push payloads in a Notification Service Extension (§7.2), verified
  against `e2e_crypto/push_vectors.json`. For direct-mode pairings, carry the
  data plane over the direct link (§5.10) and keep using the relay for push
  tokens and revoke.

---

## 13. File map

- `remote/protocol/src/ids.rs` — id newtypes.
- `remote/protocol/src/common.rs` — version constants (incl.
  `PUSH_NOTIFY_MIN_VERSION`), `Role`, `PushClass`, `AgentType`, `AgentStatus`,
  `RollupDot`, git types; `negotiate_version` lives in `lib.rs`.
- `remote/protocol/src/relay.rs` — relay plane (`RelayFrame`, `EncryptedEnvelope`,
  presence/error/push enums), `RelayMode`, `DirectEndpoint`, the push request
  types `PushNotifyBody` / `PushRequest` / `PushErrorBody`, and the shared
  canonical-string helpers `push_request_signing_message`,
  `direct_auth_message`, `push_endpoint_url` plus their constants.
- `remote/protocol/src/e2e.rs` — E2E plane (`DesktopToPhone`, `PhoneCommand`,
  transcript/event/shell/status types) and the push plaintext `PushPayload`
  with `push_aad`, `truncate_push_text` and the push size caps.
- `remote/protocol/tests/round_trip.rs` — fixture walker + invariant tests.
- `remote/protocol/tests/fixtures/{relay,desktop_to_phone,phone_to_desktop}/*.json`
  — at least one golden fixture per message variant: 31 relay files covering all
  25 `RelayFrame` variants (extra files pin optional-field shapes: v2- and
  v4-format `hello_ok`/`pairing_offer`/`pairing_claimed`, the push-only
  `error`), 10 desktop→phone, 17 phone→desktop. The relay set includes
  `machine_name`, `revoke`, `pairing_revoked` (§5.7–§5.8) and `push_notify`
  (§5.5).
- `remote/protocol/tests/fixtures/push_payload/*.json` — `PushPayload`
  plaintexts (§7.2); `push_http/*.json` — the `POST /v1/push` request and error
  bodies (§5.5.3).
- `remote/protocol/tests/fixtures/e2e_crypto/{vectors,push_vectors}.json` —
  cross-language crypto vectors (§7.1, §7.2), generated and locked by
  `src/remote/crypto.rs` tests.
