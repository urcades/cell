# Plugin Capability Matrix

This document describes the product-level boundary between the Rust-native plugin system and the older extension model it replaced.

## Live in the current Rust product

- descriptor discovery from `cell-plugin-host.json` and `plugin-host.json`
- out-of-process launch
- stdio handshake and manifest registration
- command dispatch
- tool dispatch
- lifecycle hook dispatch
- startup diagnostics and duplicate capability rejection
- plugin diagnostics through the main CLI and RPC surfaces

## Accepted but deferred

These capability classes are part of the manifest and diagnostics story, but not part of the live runtime story yet:

- plugin flags as a live CLI/runtime surface
- provider execution
- model execution

## Compatibility holdovers

These are still present, but they are not the product direction:

- project-scoped settings under `.pi/settings.json`
- some `PI_*` environment variable names
- optional TypeScript parity tooling through `PI_TS_REPO`

## Explicit non-goals

- executing JavaScript or TypeScript extensions directly
- embedding Node or Bun into the Rust runtime
- reproducing the old injected extension UI system

## Practical rule

When evaluating plugin work in this repo, judge it against the Rust-native capability classes above, not against the old JS extension runtime.
