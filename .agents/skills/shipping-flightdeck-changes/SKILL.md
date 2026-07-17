---
name: shipping-flightdeck-changes
description: Use when finishing a FlightDeck change — before committing, building a release binary, or opening/updating a pull request.
---

# Shipping FlightDeck changes

The concrete "done" checklist for this repo. Generic process (TDD, plans, review)
lives in the `superpowers` skills; this carries only the FlightDeck-specific
steps. **REQUIRED BACKGROUND:** `superpowers:finishing-a-development-branch` and
`superpowers:verification-before-completion` for the generic flow.

## Definition of done (the green bar)

A change is not done until all three pass, and you cite them:

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

- `-D warnings` and `--locked` are mandatory. Fix clippy lints in the change;
  do not `#[allow]` them away.
- Dangerous / refusal paths need tests, not only success paths (SPECS §26).
- A Stop hook runs the fast subset (`fmt --check` + `clippy`) automatically; the
  full `cargo test` run is still your responsibility.

## Hand the user a real binary

After the gate passes, **commit, then build a local release binary and point the
user to it** so they can drive the actual app before the PR is finalized. Order
is commit → build → user tests. "Tests pass" does not replace the user exercising
the real app.

```bash
cargo build --release --locked   # then tell the user the path to the binary
```

## CHANGELOG at PR time

Update `CHANGELOG.md` when **creating or updating a PR**, not on intermediate
task commits (project instruction). Group under `New features`, `Improvements`,
`Bug fixes`. `scripts/release` rolls `Unreleased` into a version at release
time — don't hand-edit released sections.

## Commits

Conventional, scoped, one logical change per commit: `feat(notify):`, `ci:`,
`build:`, `docs:`, `fix(git):`.

## PR lifecycle

1. Push the branch and open a PR against the upstream `main`.
2. **Check the PR's CI checks after pushing** — watch the Windows job especially
   (see flightdeck-cross-platform-parity). Chase red before declaring done.
3. Ask the maintainer to review when the PR is ready to be official.

> Push access varies per contributor. Whether you push to a fork and open a
> cross-repo PR or push directly depends on your own access, so the concrete
> remotes and `gh` command live in **personal (uncommitted) memory**, not in
> this committed skill. Follow your own configured workflow for that step.

## Do not commit scratch

Keep specs, implementation plans, and `superpowers`/SDD ledger files out of the
PR — they are local scratch (`docs/superpowers/**`, `.superpowers/**`). If they
slipped in, remove them before the PR.
