import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { JetBrains_Mono } from "next/font/google";

import screenshot from "../../public/screenshots/desktop/main-layout.png";

const mono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  variable: "--font-mono",
});

const MONO = "var(--font-mono), monospace";
const GITHUB_URL = "https://github.com/neworange-ruud/flightdeck";
const DOCS_URL = "/docs";

export default function HomePage() {
  return (
    <div
      className={mono.variable}
      style={{
        fontFamily: "'Helvetica Neue', Helvetica, Arial, sans-serif",
        background: "#0a0e15",
        color: "#eef4fa",
        minHeight: "100vh",
        overflow: "hidden",
        position: "relative",
      }}
    >
      {/* Ambient glow */}
      <div
        style={{
          position: "absolute",
          top: -280,
          left: "50%",
          transform: "translateX(-50%)",
          width: 1100,
          height: 620,
          background:
            "radial-gradient(ellipse at center, rgba(111,220,242,0.14), rgba(111,220,242,0) 62%)",
          pointerEvents: "none",
          zIndex: 0,
        }}
      />

      {/* NAV */}
      <nav
        style={{
          position: "sticky",
          top: 0,
          zIndex: 40,
          backdropFilter: "blur(14px)",
          background: "rgba(10,14,21,0.72)",
          borderBottom: "1px solid rgba(255,255,255,0.06)",
        }}
      >
        <div
          style={{
            maxWidth: 1180,
            margin: "0 auto",
            padding: "16px 28px",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 24,
          }}
        >
          <a href="#top" style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <svg width="34" height="34" viewBox="0 0 44 44" fill="none" aria-hidden="true">
              <defs>
                <linearGradient id="fdg" x1="0" y1="0" x2="1" y2="1">
                  <stop offset="0" stopColor="#8ee9fb" />
                  <stop offset="1" stopColor="#3fb8dd" />
                </linearGradient>
              </defs>
              <rect x="2" y="2" width="40" height="40" rx="11" fill="url(#fdg)" />
              <rect
                x="2"
                y="2"
                width="40"
                height="40"
                rx="11"
                fill="none"
                stroke="rgba(255,255,255,0.35)"
                strokeWidth="1"
              />
              <polyline
                points="13,23 22,14 31,23"
                fill="none"
                stroke="#08131b"
                strokeWidth="3.6"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
              <polyline
                points="13,31 22,22 31,31"
                fill="none"
                stroke="#08131b"
                strokeWidth="3.6"
                strokeLinecap="round"
                strokeLinejoin="round"
                opacity="0.55"
              />
            </svg>
            <span
              style={{
                fontSize: 19,
                fontWeight: 700,
                letterSpacing: "-0.02em",
                color: "#f4f8fc",
              }}
            >
              FlightDeck
            </span>
          </a>
          <div style={{ display: "flex", alignItems: "center", gap: 30 }}>
            <a href="#features" style={{ color: "#99a6b6", fontSize: 14.5, fontWeight: 500 }}>
              Features
            </a>
            <a href="#install" style={{ color: "#99a6b6", fontSize: 14.5, fontWeight: 500 }}>
              Install
            </a>
            <a
              href={GITHUB_URL}
              style={{ color: "#99a6b6", fontSize: 14.5, fontWeight: 500 }}
            >
              GitHub
            </a>
            <Link
              href={DOCS_URL}
              style={{
                display: "inline-flex",
                alignItems: "center",
                padding: "9px 18px",
                borderRadius: 999,
                background: "#6fdcf2",
                color: "#06121a",
                fontSize: 14,
                fontWeight: 700,
                letterSpacing: "-0.01em",
              }}
            >
              Docs
            </Link>
          </div>
        </div>
      </nav>

      <div id="top" />

      {/* HERO */}
      <header
        style={{
          position: "relative",
          zIndex: 1,
          maxWidth: 1180,
          margin: "0 auto",
          padding: "96px 28px 40px",
          textAlign: "center",
        }}
      >
        <div
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 10,
            padding: "7px 16px",
            borderRadius: 999,
            border: "1px solid rgba(111,220,242,0.28)",
            background: "rgba(111,220,242,0.06)",
            marginBottom: 30,
          }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: "#7ee787",
              boxShadow: "0 0 8px #7ee787",
            }}
          />
          <span
            style={{
              fontFamily: MONO,
              fontSize: 12,
              letterSpacing: "0.16em",
              color: "#7fd8ef",
              textTransform: "uppercase",
            }}
          >
            Multiple projects · Parallel agents · one cockpit
          </span>
        </div>
        <h1
          style={{
            margin: "0 auto",
            maxWidth: 900,
            fontSize: 74,
            lineHeight: 0.98,
            fontWeight: 700,
            letterSpacing: "-0.035em",
            color: "#f5f9fd",
            textWrap: "balance",
          }}
        >
          Keep every coding
          <br />
          agent on course.
        </h1>
        <p
          style={{
            margin: "30px auto 0",
            maxWidth: 616,
            fontSize: 19,
            lineHeight: 1.6,
            color: "#97a4b4",
            textWrap: "pretty",
          }}
        >
          FlightDeck is a keyboard-driven terminal workspace for running several local AI
          coding agents in parallel — each in its own Git worktree, on its own branch, in
          its own session. Supervise the whole fleet from one screen and stay in control of
          every commit, merge, and pull request.
        </p>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            gap: 14,
            marginTop: 38,
            flexWrap: "wrap",
          }}
        >
          <Link
            href={DOCS_URL}
            style={{
              display: "inline-flex",
              alignItems: "center",
              padding: "15px 28px",
              borderRadius: 999,
              background: "#6fdcf2",
              color: "#06121a",
              fontSize: 15.5,
              fontWeight: 700,
              letterSpacing: "-0.01em",
              boxShadow: "0 0 40px rgba(111,220,242,0.28)",
            }}
          >
            Read the documentation
          </Link>
          <a
            href="#install"
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 10,
              padding: "15px 24px",
              borderRadius: 999,
              border: "1px solid rgba(255,255,255,0.14)",
              background: "rgba(255,255,255,0.03)",
              color: "#dbe4ee",
              fontFamily: MONO,
              fontSize: 14,
              fontWeight: 500,
            }}
          >
            <span style={{ color: "#6fdcf2" }}>$</span> brew install flightdeck
          </a>
        </div>
        <p
          style={{
            margin: "22px 0 0",
            fontFamily: MONO,
            fontSize: 12,
            letterSpacing: "0.05em",
            color: "#5c6b7c",
          }}
        >
          macOS · Linux · Windows &nbsp;—&nbsp; free &amp; open source (MIT)
        </p>
      </header>

      {/* SCREENSHOT SHOWCASE */}
      <section
        style={{
          position: "relative",
          zIndex: 1,
          maxWidth: 1120,
          margin: "44px auto 0",
          padding: "0 28px",
        }}
      >
        <div
          style={{
            position: "relative",
            borderRadius: 14,
            overflow: "hidden",
            border: "1px solid rgba(255,255,255,0.10)",
            background: "#0b1017",
            boxShadow:
              "0 40px 120px -30px rgba(0,0,0,0.85), 0 0 0 1px rgba(111,220,242,0.05)",
          }}
        >
          <Image
            src={screenshot}
            alt="FlightDeck: project tabs across the top, one row per agent in the sidebar, the active agent's live terminal in the main pane, and a real-time Git summary along the bottom"
            placeholder="blur"
            priority
            sizes="(max-width: 1120px) 100vw, 1064px"
            style={{ display: "block", width: "100%", height: "auto" }}
          />
        </div>
        <p
          style={{
            textAlign: "center",
            margin: "18px auto 0",
            maxWidth: 720,
            fontSize: 14,
            lineHeight: 1.6,
            color: "#6b7a8c",
          }}
        >
        </p>
      </section>



      {/* FEATURES */}
      <section
        id="features"
        style={{
          position: "relative",
          zIndex: 1,
          maxWidth: 1180,
          margin: "100px auto 0",
          padding: "0 28px",
          textAlign: "center",
        }}
      >
        <h2
          style={{
            fontSize: 44,
            fontWeight: 700,
            letterSpacing: "-0.03em",
            color: "#f5f9fd",
            margin: "0 0 12px",
          }}
        >
          Run multiple agents without the chaos.
        </h2>
        <p
          style={{
            fontSize: 18,
            lineHeight: 1.6,
            color: "#8b98a8",
            margin: "0 auto 44px",
            maxWidth: 620,
            textAlign: "center",
          }}
        >
          Running one AI agent is easy. Running a fleet without them clobbering each other is
          not — so FlightDeck isolates every task and surfaces every status.
        </p>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(3, 1fr)",
            gap: 18,
          }}
        >
          {FEATURES.map((feature) => (
            <div
              key={feature.title}
              style={{
                padding: "26px 24px",
                borderRadius: 14,
                border: "1px solid rgba(255,255,255,0.08)",
                background: "rgba(255,255,255,0.02)",
              }}
            >
              <div
                style={{
                  fontFamily: MONO,
                  color: feature.iconColor,
                  fontSize: 18,
                  marginBottom: 16,
                }}
              >
                {feature.icon}
              </div>
              <h3
                style={{
                  fontSize: 18,
                  fontWeight: 700,
                  color: "#eaf1f8",
                  margin: "0 0 8px",
                  letterSpacing: "-0.01em",
                }}
              >
                {feature.title}
              </h3>
              <p style={{ fontSize: 14.5, lineHeight: 1.6, color: "#8b98a8", margin: 0 }}>
                {feature.body}
              </p>
            </div>
          ))}
        </div>
      </section>

      {/* INSTALL */}
      <section
        id="install"
        style={{
          position: "relative",
          zIndex: 1,
          maxWidth: 1180,
          margin: "100px auto 0",
          padding: "0 28px",
        }}
      >
        <h2
          style={{
            fontSize: 44,
            fontWeight: 700,
            letterSpacing: "-0.03em",
            color: "#f5f9fd",
            margin: "0 0 12px",
            textAlign: "center",
          }}
        >
          Installs in one command.
        </h2>
        <p
          style={{
            fontSize: 18,
            lineHeight: 1.6,
            color: "#8b98a8",
            margin: "0 auto 44px",
            maxWidth: 560,
            textAlign: "center",
          }}
        >
          Install, drop into any Git repo, and run{" "}
          <span style={{ fontFamily: MONO, color: "#a7b4c4" }}>flightdeck</span>. It
          auto-initializes on first launch — no config required.
        </p>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(3, 1fr)",
            gap: 18,
          }}
        >
          <InstallCard label="HOMEBREW">
            <span style={{ color: "#6fdcf2" }}>$</span> brew install \
            <br />
            &nbsp;&nbsp;neworange-ruud/tap/flightdeck
          </InstallCard>
          <InstallCard label="macOS / LINUX">
            <span style={{ color: "#6fdcf2" }}>$</span> curl -LsSf \
            <br />
            &nbsp;&nbsp;<span style={{ color: "#8b98a8" }}>.../flightdeck-installer.sh</span> \
            <br />
            &nbsp;&nbsp;| sh
          </InstallCard>
          <InstallCard label="WINDOWS">
            <span style={{ color: "#6fdcf2" }}>&gt;</span> irm{" "}
            <span style={{ color: "#8b98a8" }}>.../installer.ps1</span> \
            <br />
            &nbsp;&nbsp;| iex
          </InstallCard>
        </div>
        <div style={{ textAlign: "center", marginTop: 40 }}>
          <Link
            href={DOCS_URL}
            style={{
              display: "inline-flex",
              alignItems: "center",
              padding: "15px 30px",
              borderRadius: 999,
              background: "#6fdcf2",
              color: "#06121a",
              fontSize: 15.5,
              fontWeight: 700,
              letterSpacing: "-0.01em",
              boxShadow: "0 0 40px rgba(111,220,242,0.28)",
            }}
          >
            Read the documentation
          </Link>
        </div>
      </section>

      {/* FOOTER */}
      <footer
        style={{
          position: "relative",
          zIndex: 1,
          maxWidth: 1180,
          margin: "100px auto 0",
          padding: "40px 28px 56px",
          borderTop: "1px solid rgba(255,255,255,0.06)",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 24,
          flexWrap: "wrap",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 11 }}>
          <svg width="26" height="26" viewBox="0 0 44 44" fill="none" aria-hidden="true">
            <rect x="2" y="2" width="40" height="40" rx="11" fill="url(#fdg)" />
            <polyline
              points="13,23 22,14 31,23"
              fill="none"
              stroke="#08131b"
              strokeWidth="3.6"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
            <polyline
              points="13,31 22,22 31,31"
              fill="none"
              stroke="#08131b"
              strokeWidth="3.6"
              strokeLinecap="round"
              strokeLinejoin="round"
              opacity="0.55"
            />
          </svg>
          <span style={{ fontSize: 15, fontWeight: 700, color: "#dbe4ee" }}>FlightDeck</span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 26, fontSize: 14 }}>
          <a href="#features" style={{ color: "#8b98a8" }}>
            Features
          </a>
          <a href="#install" style={{ color: "#8b98a8" }}>
            Install
          </a>
          <Link href={DOCS_URL} style={{ color: "#8b98a8" }}>
            Documentation
          </Link>
          <a href={GITHUB_URL} style={{ color: "#8b98a8" }}>
            GitHub
          </a>
        </div>
        <div style={{ fontFamily: MONO, fontSize: 11.5, color: "#4f6070" }}>
          MIT · macOS · Linux · Windows
        </div>
      </footer>
    </div>
  );
}

function InstallCard({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div
      style={{
        borderRadius: 12,
        border: "1px solid rgba(255,255,255,0.08)",
        background: "#0b1017",
        overflow: "hidden",
      }}
    >
      <div
        style={{
          padding: "11px 16px",
          borderBottom: "1px solid rgba(255,255,255,0.06)",
          fontFamily: MONO,
          fontSize: 11,
          letterSpacing: "0.1em",
          color: "#6fdcf2",
        }}
      >
        {label}
      </div>
      <div
        style={{
          padding: "18px 16px",
          fontFamily: MONO,
          fontSize: 12.5,
          lineHeight: 1.7,
          color: "#cdd8e4",
        }}
      >
        {children}
      </div>
    </div>
  );
}

const FEATURES = [
  {
    icon: "⎇",
    iconColor: "#6fdcf2",
    title: "Isolated Git worktrees",
    body: (
      <>
        Each agent gets its own worktree and branch under{" "}
        <span style={{ fontFamily: MONO, color: "#a7b4c4" }}>.flightdeck/</span>. Agents work
        simultaneously and never touch each other&apos;s changes.
      </>
    ),
  },
  {
    icon: "⌘",
    iconColor: "#6fdcf2",
    title: "Keyboard-first control",
    body: (
      <>
        Two modes, one dependable fallback: the command palette on{" "}
        <span style={{ fontFamily: MONO, color: "#a7b4c4" }}>Ctrl-g</span>. Switch projects,
        spawn tabs, push branches — all without leaving the keyboard.
      </>
    ),
  },
  {
    icon: "●",
    iconColor: "#7ee787",
    title: "Live agent status",
    body: "Every tab shows whether its agent is working, idle, or waiting — from real lifecycle hooks, never guessed from terminal output.",
  },
  {
    icon: "▤",
    iconColor: "#6fdcf2",
    title: "Multiple projects",
    body: "Open several repos at once. Every project stays live in the background — its agents keep running and still notify while you look elsewhere.",
  },
  {
    icon: "⬢",
    iconColor: "#6fdcf2",
    title: "Sandboxed containers",
    body: "Optionally run each agent inside a rootless Podman container with non-disableable guardrails — the host keeps owning every Git operation.",
  },
  {
    icon: "◈",
    iconColor: "#6fdcf2",
    title: "FlightDeck Remote",
    body: "Pair your phone to check in on running sessions, chat with an agent, and get pinged the moment one finishes or needs you.",
  },
];
