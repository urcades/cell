# Cell Rust Port Postmortem

This document is the historical architecture review of the Rust port after the broad parity program and productization work were completed.

## Executive summary

The Rust port succeeded.

`cell` is no longer a sidecar experiment or a parity prototype. It is a standalone Rust product with:

- a working CLI and terminal UI
- session persistence and branching
- model selection and auth-backed provider support
- resource and package handling
- export and share flows
- a Rust-native plugin system with live commands, tools, hooks, and diagnostics
- Rust-only verification, packaging, and release workflow

The important architectural judgment is this:

- the Rust repo preserved the product behavior that mattered
- it did not preserve the TypeScript architecture one-for-one
- in several subsystems, the Rust decomposition is cleaner than the original TypeScript split
- in a few places, especially large interactive orchestration code, the Rust tree still carries complexity from the parity era

## What the port got right

### Clear crate seams

The Rust repo decomposes the product into narrower crates for:

- core runtime
- sessions
- config
- resources
- packages
- models
- OAuth
- tools
- TUI
- plugin host and protocol

That makes it easier to reason about the product by subsystem than the original mixed TypeScript layout.

### Serious verification discipline

The Rust port did not stop at “it seems close enough.”

It built and kept:

- full workspace test coverage
- a Rust-only terminal regression suite
- Rust-native packaging and release scripts
- plugin host and example plugin tests

### A correct stop rule

The port stopped chasing the wrong kind of parity.

The team correctly treated these as explicit non-goals rather than unfinished bugs:

- direct JS/TS extension execution
- Node/Bun embedding
- custom injected extension UI

That boundary is one of the strongest architectural decisions in the repo.

## What still feels costly

### The interactive layer remains the largest maintenance hotspot

The CLI interactive surface is much healthier than it was before the hardening work, but it is still one of the densest areas in the tree. Future cleanup work should keep shrinking accidental complexity there rather than adding new logic back into it.

### Compatibility holdovers still exist

The product name is now `cell`, but some compatibility surfaces still use older names:

- `.pi/settings.json`
- `PI_*` environment variables
- `PI_TS_REPO` for optional parity tooling

Those are not blockers, but they are part of the remaining cleanup story if the project ever wants a deeper branding cut.

### Historical docs can mislead if not marked as historical

Earlier versions of the repo documentation mixed active guidance with old planning documents. That is why the current docs now split operating guides from archived history more clearly.

## Largest architectural differences from the TypeScript original

### Terminal implementation

The visible terminal behavior was preserved, but the implementation was rebuilt as a Rust-native renderer and widget system rather than a library-for-library port.

### Session, package, and resource pipeline

The Rust version uses a cleaner seam split across dedicated crates rather than keeping those concerns intertwined.

### Plugin boundary

The Rust runtime now has a real out-of-process plugin system. It does not try to mimic the old JS extension runtime literally.

### Release ownership

The Rust repo now owns its own verification, packaging, tags, and release history.

## What remains future work rather than unfinished parity

These are separate future tracks, not evidence that the port failed:

- plugin-provided provider execution
- plugin-provided model execution
- deeper cleanup of older compatibility names and paths
- broader multi-platform release automation

## Bottom line

The port should be judged as successful.

It reached the right end state:

- a real standalone Rust product
- a real Rust-native plugin system
- a clear boundary around what is supported today
- a clear boundary around what is intentionally not part of the product
