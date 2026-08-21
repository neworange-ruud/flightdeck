import type { Metadata } from "next";
import Link from "next/link";
import type { ReactNode } from "react";
import { JetBrains_Mono } from "next/font/google";

const mono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  variable: "--font-mono",
});

const MONO = "var(--font-mono), monospace";

/// Bumped whenever the substance of this policy changes — App Store Connect and
/// the in-app link both point here, so the date is the user-visible version.
const LAST_UPDATED = "25 July 2026";

const CONTACT_EMAIL = "privacy@flightdeckai.app";

export const metadata: Metadata = {
  title: "Privacy Policy · FlightDeck",
  description:
    "How FlightDeck and FlightDeck Remote handle your data: end-to-end encrypted messages, a zero-knowledge relay, and no analytics or tracking.",
};

export default function PrivacyPage() {
  return (
    <div
      className={mono.variable}
      style={{
        fontFamily: "'Helvetica Neue', Helvetica, Arial, sans-serif",
        background: "#0a0e15",
        color: "#eef4fa",
        minHeight: "100vh",
      }}
    >
      <main
        style={{
          maxWidth: 760,
          margin: "0 auto",
          padding: "64px 28px 96px",
          fontSize: 15.5,
          lineHeight: 1.75,
          color: "#cdd8e4",
        }}
      >
        <Link
          href="/"
          style={{ fontFamily: MONO, fontSize: 12.5, color: "#6fdcf2", letterSpacing: "0.06em" }}
        >
          ← FlightDeck
        </Link>

        <h1
          style={{
            fontSize: 38,
            fontWeight: 700,
            color: "#eef4fa",
            letterSpacing: "-0.02em",
            margin: "26px 0 10px",
          }}
        >
          Privacy Policy
        </h1>
        <p style={{ fontFamily: MONO, fontSize: 12.5, color: "#6b7c8d", marginBottom: 40 }}>
          Last updated {LAST_UPDATED}
        </p>

        <Callout>
          <strong style={{ color: "#eef4fa" }}>The short version.</strong> FlightDeck on your
          computer sends us nothing at all. If you pair the FlightDeck Remote app, the messages
          between your phone and your computer are end-to-end encrypted — our relay passes
          ciphertext it cannot read. The relay does see connection metadata, including your
          computer&apos;s name. There is no analytics, no tracking, and no advertising anywhere in
          FlightDeck, and we never sell your data.
        </Callout>

        <H2>Who we are</H2>
        <P>
          FlightDeck and FlightDeck Remote are developed and operated by DotTech Holding B.V., a
          company registered in the Netherlands. For anything in this policy — including requests
          about your personal data — contact us at{" "}
          <A href={`mailto:${CONTACT_EMAIL}`}>{CONTACT_EMAIL}</A>.
        </P>
        <P>
          For the purposes of the EU General Data Protection Regulation (GDPR), DotTech Holding
          B.V. is the data controller for the limited personal data described below.
        </P>

        <H2>What this policy covers</H2>
        <P>This policy covers three things:</P>
        <Ul>
          <Li>
            <B>FlightDeck</B> — the desktop application that runs coding agents on your own
            computer.
          </Li>
          <Li>
            <B>FlightDeck Remote</B> — the companion iOS app that pairs with your computer.
          </Li>
          <Li>
            <B>The FlightDeck relay</B> — the service we operate that forwards encrypted messages
            between the two.
          </Li>
        </Ul>

        <H2>FlightDeck (desktop)</H2>
        <P>
          The desktop application collects nothing and sends us nothing. It has no analytics, no
          crash reporting, no update pings, and no accounts. Your code, your repositories, and
          your agent conversations stay on your machine.
        </P>
        <P>
          Two kinds of network traffic do leave your computer, and neither goes to us. First, the
          coding agent you choose to run (for example Claude Code) talks directly to its own
          provider, under that provider&apos;s terms and privacy policy — FlightDeck does not
          intermediate, inspect, or copy that traffic. Second, if you choose to enable FlightDeck
          Remote, your computer connects to our relay as described below. Remote is entirely
          opt-in; if you never pair a phone, your computer never contacts us.
        </P>

        <H2>FlightDeck Remote (iOS)</H2>
        <P>There are no accounts and no sign-up. You pair the app to your own computer instead.</P>

        <H3>Stored on your device</H3>
        <Ul>
          <Li>
            <B>Cryptographic keys</B> — your device identity keypair and key-agreement keys, held
            in the iOS Keychain. They never leave your device.
          </Li>
          <Li>
            <B>Pairing records</B> — which computers you are paired with, their names, and when
            pairing happened.
          </Li>
          <Li>
            <B>A relay access credential</B>, if your relay requires one, held in the Keychain.
          </Li>
          <Li>
            <B>Your preferences</B> — app-lock setting, dictation language, notification choices.
          </Li>
          <Li>
            <B>A local cache</B> of the most recent session state and transcripts, so the app can
            show you something before it reconnects. This lives in the app&apos;s own sandboxed
            storage and is removed when you delete the app.
          </Li>
        </Ul>

        <H3>Camera</H3>
        <P>
          The camera is used for one purpose: scanning the pairing QR code shown on your computer.
          Frames are processed on your device to read the code. No image is stored, and none is
          transmitted anywhere.
        </P>

        <H3>Microphone and voice dictation</H3>
        <P>
          If you hold the microphone button to dictate a reply, the app records audio and passes it
          to Apple&apos;s speech recognition framework to turn it into text.{" "}
          <B>
            This transcription is not guaranteed to happen on your device — Apple may send the
            audio to its servers to process it
          </B>
          , which is governed by{" "}
          <A href="https://www.apple.com/legal/privacy/">Apple&apos;s privacy policy</A>. The audio
          is not sent to us and we do not store it. The resulting text stays in the compose box
          for you to review, and is only sent to your computer — end-to-end encrypted — when you
          choose to send it.
        </P>

        <H2>The relay: what it can and cannot see</H2>
        <P>
          Your phone and your computer establish a shared secret directly with each other and
          encrypt every message with it (X25519 key agreement, HKDF-SHA256 key derivation, and
          ChaCha20-Poly1305 authenticated encryption, all via Apple&apos;s CryptoKit and its Rust
          counterpart). The relay only ever holds the resulting ciphertext.
        </P>
        <P>
          <B>The relay cannot read:</B> your messages, your agents&apos; output, your prompts, your
          code, your file paths, your terminal contents, or your repository names.
        </P>
        <P>
          <B>The relay does see and store:</B>
        </P>
        <Ul>
          <Li>
            Device identifiers and public keys for your phone and computer, so it can route and
            authenticate messages.
          </Li>
          <Li>
            Pairing identifiers, and{" "}
            <B>the name of your computer</B> — which is usually a device name you chose and may
            contain your own name (for example &ldquo;Ruud&apos;s MacBook Pro&rdquo;). This is
            stored unencrypted so the app can show you which machine is which.
          </Li>
          <Li>
            Short-lived, single-use pairing tokens (they expire about two minutes after being
            issued).
          </Li>
          <Li>
            Your Apple push notification token, if you enable notifications, so it can wake the
            app.
          </Li>
          <Li>
            Encrypted message envelopes awaiting delivery, plus their timing, ordering, and size.
            Even though the contents are unreadable, this metadata reveals when you were active
            and roughly how much traffic there was.
          </Li>
        </Ul>
        <P>
          The relay&apos;s operational logs record connection, device and pairing identifiers and
          pairing tokens — never message contents. The relay runs on Microsoft Azure, whose
          platform logging records network-level connection metadata including IP addresses. Azure
          acts as our data processor.
        </P>

        <H2>Push notifications</H2>
        <P>
          When an agent needs you and your phone is not connected, the relay asks Apple to wake the
          app. <B>That push carries no content whatsoever</B> — it is an empty
          &ldquo;wake up&rdquo; signal. The app wakes, reconnects, decrypts the messages waiting
          for it, and writes the notification you actually see on your own device. The notification
          text is therefore composed locally and never passes through Apple or through us.
        </P>
        <P>
          Apple sees your push token and the fact that a push was sent, as described in
          Apple&apos;s privacy policy. You can revoke this at any time in iOS notification
          settings, or by turning notifications off for a machine in the app.
        </P>

        <H2>What we do not do</H2>
        <Ul>
          <Li>No analytics or usage tracking, of any kind, in either app.</Li>
          <Li>No advertising, and no advertising identifiers. The app does not track you across apps or websites.</Li>
          <Li>No third-party analytics, attribution, or crash-reporting SDKs.</Li>
          <Li>No selling or sharing of personal data. There is nothing to sell.</Li>
          <Li>No profiling and no automated decision-making.</Li>
        </Ul>

        <H2>How long we keep things</H2>
        <Ul>
          <Li>
            <B>Encrypted messages awaiting delivery</B> are deleted as soon as the recipient
            confirms receipt. Undelivered messages are capped per pairing (1,000 by default) and
            the oldest are dropped past that limit.
          </Li>
          <Li>
            <B>Pairing tokens</B> expire roughly two minutes after being issued and can only be
            used once.
          </Li>
          <Li>
            <B>Pairings, device keys and push tokens</B> are kept while the pairing exists. Unpair
            in the app — or delete the app — and they are removed from the relay.
          </Li>
          <Li>
            <B>Operational logs</B> are retained for a limited period for reliability and abuse
            prevention, then deleted.
          </Li>
        </Ul>

        <H2>Legal basis</H2>
        <P>
          Where GDPR applies, we process the limited data above to perform the service you asked
          for — connecting your phone to your computer (Article 6(1)(b), performance of a
          contract) — and to keep that service secure and reliable (Article 6(1)(f), legitimate
          interests). Camera, microphone, speech recognition and notification access are each
          requested through iOS permission prompts and are entirely optional; the app works
          without them.
        </P>

        <H2>Your rights</H2>
        <P>
          You have the right to access, correct, delete, restrict, and object to the processing of
          your personal data, and to data portability. Because we hold so little, the most
          effective control is in your hands directly: unpairing a machine, or deleting the app,
          removes your device keys, pairing and push token from the relay. For anything else, write
          to <A href={`mailto:${CONTACT_EMAIL}`}>{CONTACT_EMAIL}</A> and we will respond within one
          month.
        </P>
        <P>
          If you are in the EU or EEA and you believe we have handled your data improperly, you can
          lodge a complaint with your local supervisory authority. In the Netherlands that is the{" "}
          <A href="https://autoriteitpersoonsgegevens.nl/">Autoriteit Persoonsgegevens</A>.
        </P>

        <H2>International transfers</H2>
        <P>
          The relay is hosted on Microsoft Azure. Push notifications are delivered by Apple, and
          speech recognition may be processed by Apple, both of which may involve processing
          outside the EEA under their own safeguards and standard contractual clauses.
        </P>

        <H2>Children</H2>
        <P>
          FlightDeck is a developer tool and is not directed at children. We do not knowingly
          collect personal data from anyone under 16.
        </P>

        <H2>Changes to this policy</H2>
        <P>
          If we change this policy we will update the date at the top of this page. Material
          changes to how the relay handles your data will also be noted in the FlightDeck release
          notes.
        </P>

        <H2>Contact</H2>
        <P>
          DotTech Holding B.V., the Netherlands —{" "}
          <A href={`mailto:${CONTACT_EMAIL}`}>{CONTACT_EMAIL}</A>
        </P>

        <div
          style={{
            marginTop: 56,
            paddingTop: 24,
            borderTop: "1px solid rgba(255,255,255,0.08)",
            fontFamily: MONO,
            fontSize: 12,
            color: "#4f6070",
          }}
        >
          FlightDeck is open source under the MIT licence — the behaviour described above is
          verifiable in{" "}
          <A href="https://github.com/neworange-ruud/flightdeck">the source</A>.
        </div>
      </main>
    </div>
  );
}

function H2({ children }: { children: ReactNode }) {
  return (
    <h2
      style={{
        fontSize: 22,
        fontWeight: 700,
        color: "#eef4fa",
        letterSpacing: "-0.01em",
        margin: "44px 0 12px",
      }}
    >
      {children}
    </h2>
  );
}

function H3({ children }: { children: ReactNode }) {
  return (
    <h3
      style={{
        fontFamily: MONO,
        fontSize: 13,
        fontWeight: 500,
        color: "#6fdcf2",
        letterSpacing: "0.08em",
        textTransform: "uppercase",
        margin: "30px 0 10px",
      }}
    >
      {children}
    </h3>
  );
}

function P({ children }: { children: ReactNode }) {
  return <p style={{ margin: "0 0 16px" }}>{children}</p>;
}

function Ul({ children }: { children: ReactNode }) {
  return (
    <ul style={{ margin: "0 0 16px", paddingLeft: 22, display: "grid", gap: 9 }}>{children}</ul>
  );
}

function Li({ children }: { children: ReactNode }) {
  return <li style={{ paddingLeft: 4 }}>{children}</li>;
}

function B({ children }: { children: ReactNode }) {
  return <strong style={{ color: "#dbe4ee", fontWeight: 600 }}>{children}</strong>;
}

function A({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a href={href} style={{ color: "#6fdcf2", textDecoration: "underline" }}>
      {children}
    </a>
  );
}

function Callout({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        background: "rgba(111,220,242,0.05)",
        border: "1px solid rgba(111,220,242,0.16)",
        borderRadius: 12,
        padding: "18px 20px",
        margin: "0 0 8px",
        fontSize: 15,
      }}
    >
      {children}
    </div>
  );
}
