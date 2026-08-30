import { startHost, stopHost } from "./support/host";

/**
 * One real FlightDeck for the whole run.
 *
 * One, not one per test, for two reasons. Booting the desktop costs a PTY spawn,
 * first-run config seeding and an agent launch — seconds, not milliseconds — and
 * more importantly D14 as revised gives out one **input lock** at a time: two
 * browsers typing at once would refuse each other's keystrokes and raise
 * takeover screens no test asked for. (Seats themselves are no longer scarce —
 * both browsers would be writers — but the turn to type is, which is the same
 * problem for a parallel suite.) The suite is therefore serial (`workers: 1` in
 * `playwright.config.ts`), and each test authenticates from a clean browser
 * context against the same host.
 */
export default async function globalSetup(): Promise<void> {
  /** A leftover host from an interrupted run would hold the port. */
  stopHost();
  const host = await startHost();
  console.log(
    `[e2e] FlightDeck is up at ${host.baseURL} (pid ${host.pid}); fixture ${host.fixtureDir}`,
  );
}
