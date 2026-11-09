import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { JetBrains_Mono } from "next/font/google";

import screenshot from "../../public/screenshots/desktop-app/worker-window.png";

const mono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  variable: "--font-mono",
});

const MONO = "var(--font-mono), monospace";
const GITHUB_URL = "https://github.com/neworange-ruud/flightdeck";
const DOCS_URL = "/docs";
const PRIVACY_URL = "/privacy";
const DESKTOP_RELEASES_URL =
  "https://github.com/neworange-ruud/flightdeck/releases?q=desktop&expanded=true";

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
            <a href="#setups" style={{ color: "#99a6b6", fontSize: 14.5, fontWeight: 500 }}>
              How it runs
            </a>
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
            Parallel agents · always-on workers · one window
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
          FlightDeck runs several AI coding agents side by side, each on its own Git branch
          in its own worktree, and shows all of them in one window. Run them on your
          laptop, or hand them to an always-on worker and keep FlightDeck Desktop as the
          screen. You stay in control of every commit, merge and pull request.
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
            Get started
          </Link>
          <a
            href={DESKTOP_RELEASES_URL}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 10,
              padding: "15px 24px",
              borderRadius: 999,
              border: "1px solid rgba(255,255,255,0.14)",
              background: "rgba(255,255,255,0.03)",
              color: "#dbe4ee",
              fontSize: 15,
              fontWeight: 600,
            }}
          >
            <span style={{ color: "#6fdcf2" }}>↓</span> Download FlightDeck Desktop
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
            alt="FlightDeck Desktop connected to a worker: one row per agent with its live status, the selected agent's terminal, and the branch's git state along the bottom"
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
          FlightDeck Desktop driving a worker: one row per agent with its live status, the
          selected agent&apos;s terminal, and the branch&apos;s git state along the bottom.
        </p>
      </section>

      {/* SETUPS */}
      <section
        id="setups"
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
          Two ways to run it.
        </h2>
        <p
          style={{
            fontSize: 18,
            lineHeight: 1.6,
            color: "#8b98a8",
            margin: "0 auto 44px",
            maxWidth: 620,
          }}
        >
          Same app, same shortcuts. The only difference is where the agents run, and you
          can switch any time.
        </p>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))",
            gap: 18,
            textAlign: "left",
          }}
        >
          {SETUPS.map((setup) => (
            <Link
              key={setup.title}
              href={setup.href}
              style={{
                display: "block",
                padding: "28px 26px",
                borderRadius: 14,
                border: setup.recommended
                  ? "1px solid rgba(111,220,242,0.45)"
                  : "1px solid rgba(255,255,255,0.08)",
                background: setup.recommended
                  ? "rgba(111,220,242,0.05)"
                  : "rgba(255,255,255,0.02)",
                color: "inherit",
              }}
            >
              <div
                style={{
                  fontFamily: MONO,
                  fontSize: 11,
                  letterSpacing: "0.12em",
                  color: setup.recommended ? "#6fdcf2" : "#6b7a8c",
                  marginBottom: 12,
                  textTransform: "uppercase",
                }}
              >
                {setup.kicker}
              </div>
              <h3
                style={{
                  fontSize: 22,
                  fontWeight: 700,
                  color: "#eaf1f8",
                  margin: "0 0 10px",
                  letterSpacing: "-0.02em",
                }}
              >
                {setup.title}
              </h3>
              <p style={{ fontSize: 15, lineHeight: 1.6, color: "#8b98a8", margin: "0 0 18px" }}>
                {setup.body}
              </p>
              <ol
                style={{
                  margin: 0,
                  paddingLeft: 20,
                  listStyle: "decimal",
                  fontSize: 14.5,
                  lineHeight: 1.8,
                  color: "#b4c0cd",
                }}
              >
                {setup.steps.map((step) => (
                  <li key={step}>{step}</li>
                ))}
              </ol>
              <div style={{ marginTop: 20, fontSize: 14.5, fontWeight: 700, color: "#6fdcf2" }}>
                {setup.cta} →
              </div>
            </Link>
          ))}
        </div>
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
          Up and running in minutes.
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
          Download the Desktop app, open any Git repository and press{" "}
          <span style={{ fontFamily: MONO, color: "#a7b4c4" }}>Ctrl-n</span>. No config
          required. Install the CLI on the machine you want as a worker, or to use
          FlightDeck from a terminal.
        </p>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(300px, 1fr))",
            gap: 18,
          }}
        >
          <InstallCard label="FLIGHTDECK DESKTOP · macOS · WINDOWS · LINUX">
            <a href={DESKTOP_RELEASES_URL} style={{ color: "#6fdcf2", fontWeight: 700 }}>
              ↓ Download from GitHub Releases
            </a>
            <br />
            <span style={{ color: "#8b98a8" }}>.zip (macOS) · .msi (Windows) · .deb .rpm .AppImage</span>
          </InstallCard>
          <InstallCard label="CLI · WORKER & TERMINAL UI">
            <span style={{ color: "#6fdcf2" }}>$</span> brew install neworange-ruud/tap/flightdeck
            <br />
            <span style={{ color: "#8b98a8" }}># or: curl -LsSf .../flightdeck-installer.sh | sh</span>
            <br />
            <span style={{ color: "#8b98a8" }}># Windows: irm .../flightdeck-installer.ps1 | iex</span>
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
          <Link href={PRIVACY_URL} style={{ color: "#8b98a8" }}>
            Privacy
          </Link>
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
          overflowWrap: "anywhere",
        }}
      >
        {children}
      </div>
    </div>
  );
}

const SETUPS = [
  {
    kicker: "Recommended",
    recommended: true,
    title: "Desktop + a worker",
    body: "Agents run on an always-on Mac or Linux machine. Your laptop is only the screen: close the lid and they keep working.",
    steps: [
      "flightdeck worker start on the worker",
      "Pair FlightDeck Desktop with a 4-digit code",
      "Open a project and start agents as usual",
    ],
    cta: "Quickstart: Worker",
    href: "/docs/worker",
  },
  {
    kicker: "Simplest",
    recommended: false,
    title: "Desktop on its own",
    body: "Agents run on your own computer. Nothing else to set up, and you can add a worker later.",
    steps: [
      "Download FlightDeck Desktop",
      "Open any Git repository",
      "Press Ctrl-n, pick an agent, name the task",
    ],
    cta: "Quickstart: Desktop",
    href: "/docs/getting-started",
  },
];

const FEATURES = [
  {
    icon: "⎇",
    iconColor: "#6fdcf2",
    title: "One branch per agent",
    body: (
      <>
        Each agent gets its own Git worktree and branch under{" "}
        <span style={{ fontFamily: MONO, color: "#a7b4c4" }}>.flightdeck/</span>. Agents work
        at the same time and never touch each other&apos;s files.
      </>
    ),
  },
  {
    icon: "◉",
    iconColor: "#6fdcf2",
    title: "Always-on workers",
    body: "Put the agents on a spare Mac or Linux box over Tailscale. The Desktop is the screen, and it still notifies you when the window is closed.",
  },
  {
    icon: "●",
    iconColor: "#7ee787",
    title: "Know who needs you",
    body: "Every agent shows whether it is working, waiting or done, from the agent's own lifecycle events. Mission control puts the ones waiting on you first.",
  },
  {
    icon: "✓",
    iconColor: "#6fdcf2",
    title: "You own the history",
    body: "FlightDeck never commits, rewrites history, force-pushes or opens pull requests. It pushes your branch when you ask and hands you the PR link.",
  },
  {
    icon: "⌘",
    iconColor: "#6fdcf2",
    title: "Any agent, any keyboard",
    body: "Claude Code, Codex, OpenCode and Cursor out of the box, or add your own. Every action is one shortcut or one search away in the command palette.",
  },
  {
    icon: "+",
    iconColor: "#6fdcf2",
    title: "Room to grow",
    body: "When you need more: run agents in rootless containers, control FlightDeck from a browser or your iPhone, and hook into every worktree's lifecycle.",
  },
];
