---
name: flightdeck-config-conventions
description: Use when adding or changing a FlightDeck user-facing setting — anything in config.toml, the layered global/project config, or the configuration manager.
---

# FlightDeck config conventions

New user-visible behavior is introduced **as a setting**, not hard-coded, with an
explicit default the user chooses.

## Layered global → project

Config layers: a per-user global at `~/.flightdeck/config.toml` (created on first
run with every setting present and documented) and a per-project
`.flightdeck/config.toml` that stores **only overrides**. The project layer wins
field-by-field, **except `[agents]`**, which a project replaces wholesale when it
defines any of its own (SPECS §8).

- Behavior that reads config must read the *effective* (merged) value, and
  reload when config is saved.
- New settings must be present + documented in the generated global config.

## Back-compat: per-field defaults

Old and partial configs must keep working. Use **per-field** `#[serde(default)]`
(see `src/contracts/domain.rs`), not a struct-level `#[serde(default)]` — an
over-broad struct default has been caught in review as a regression (it wipes
sibling fields on partial input). Add a field → give it a field-level default
(e.g. `#[serde(default = "default_true")]`).

## Configuration manager

Common settings are also editable from the "Open Configuration" palette command
(toggles/choices; `Tab` switches Global/Project scope, `c` clears a project
override, `s` saves, `e` opens raw `config.toml`). If a new setting is a simple
toggle/choice, surface it there too; the full surface stays in raw TOML.

## Document it

A new setting is documented in `SPECS.md` (the numbered section for its area) and
noted in `CHANGELOG.md` at PR time (see shipping-flightdeck-changes).
