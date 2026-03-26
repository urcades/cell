# Parity Residuals (Archived)

This file is the archived stop-rule record for the broad parity program.

## Outcome

The broad parity program is closed.

The Rust repo now stands on its own as the `cell` product repo. The remaining differences are no longer active parity debt in the original sense.

## Closed as part of the parity and productization work

- Rust-only verification and packaging are in place
- the Rust terminal regression suite is part of the normal verification flow
- resource and package precedence behavior is stable
- model listing now has separate normal and diagnostic views
- the Rust plugin host is live for commands, tools, and hooks

## Remaining compatibility holdovers

These are real differences, but they are compatibility surfaces rather than broad parity blockers:

- project-scoped settings still live under `.pi/settings.json`
- some environment variable names still use `PI_*`
- TypeScript parity tooling still uses `PI_TS_REPO`

## Deferred future tracks

These are future product work, not unfinished parity:

- plugin-provided provider execution
- plugin-provided model execution
- broader multi-platform release automation

## Inherent mismatch

These are not goals for the Rust product:

- direct JS/TS extension execution
- embedding Node or Bun into the runtime
- reproducing the old injected extension UI model
