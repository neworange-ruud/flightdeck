import Link from "next/link";

export default function NotFound() {
  return <main className="grid min-h-screen place-items-center bg-[var(--landing-bg)] px-6 text-[var(--landing-fg)]"><div><p className="text-sm font-semibold tracking-[0.16em] text-[var(--landing-accent)] uppercase">404</p><h1 className="mt-3 text-4xl font-semibold">This page does not exist.</h1><Link className="mt-6 inline-block font-semibold text-[var(--landing-accent)]" href="/">Return home</Link></div></main>;
}
