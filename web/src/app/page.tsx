import Link from "next/link";

export default function HomePage() {
  return (
    <main className="grid min-h-screen place-items-center bg-[var(--landing-bg)] px-6 text-[var(--landing-fg)]">
      <section className="max-w-2xl py-20">
        <p className="text-sm font-semibold tracking-[0.2em] text-[var(--landing-accent)] uppercase">
          Flightdeck
        </p>
        <h1 className="mt-5 text-5xl font-semibold tracking-tight sm:text-7xl">
          Keep every coding agent on course.
        </h1>
        <p className="mt-6 max-w-xl text-lg leading-8 text-[var(--landing-muted)]">
          Flightdeck is a terminal workspace for running AI coding agents in
          parallel, each in its own Git worktree.
        </p>
        <Link
          className="mt-10 inline-flex rounded-full bg-[var(--landing-accent)] px-5 py-3 font-semibold text-slate-950 transition hover:bg-white"
          href="/docs"
        >
          Read the documentation
        </Link>
      </section>
    </main>
  );
}
