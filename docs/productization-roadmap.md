# Rust Pi Productization Roadmap

## Status

The Rust port is now past broad parity work.

- Apple Terminal baseline is frozen behind the existing Rust-only PTY suite.
- TypeScript remains useful as a reference and regression oracle, but it is no longer the program goal.
- The next work should be justified by product quality, maintainability, or Rust-native capability growth.

## Project 1: Baseline Closure

Status: complete

- The Rust-only PTY suite is green.
- The `share-missing-gh` check is no longer a blocking regression.
- The remaining parity residuals are now split into either normal cleanup work or separate future tracks.

## Project 2: Product Hardening

Status: started

Completed foundations:

- Settings writes now use a lock file, atomic file replacement, and read-modify-write merging against the latest on-disk state.
- Concurrent settings updates are covered by tests in `crates/pi-rust-config/src/lib.rs`.

Remaining work:

- Break `crates/pi-rust-cli/src/interactive.rs` into stable submodules.
- Move more persistent settings behavior out of UI code and into config/runtime helpers.
- Remove parity-only helpers and dead transitional paths.

## Project 3: Resource And Model Platform

Status: started

Completed foundations:

- Resource discovery now stops ancestor `.agents/skills` traversal at the workspace boundary.
- `package.json` manifests must be strict JSON before they are accepted.
- Package manifests can express resource filters and empty-array disable semantics.
- The built-in model catalog now lives in a Rust-owned generated artifact:
  - `crates/pi-rust-models/src/generated_catalog.rs`
- The model registry now exposes separate views for:
  - known models
  - available models

Remaining work:

- Finish wiring the known-vs-available split through all CLI and TUI surfaces.
- Replace the seeded built-in model artifact with a maintained generation pipeline.
- Finish resource toggle/writeback coverage for every supported resource type.

## Project 4: Headless Plugin Host v1

Status: foundation in place

Completed foundations:

- `crates/pi-rust-plugins` remains the declarative contract crate.
- `crates/pi-rust-plugin-host` now provides a first out-of-process host skeleton:
  - descriptor discovery
  - process launch
  - typed JSON-line handshake
  - registration parsing
  - duplicate capability rejection
  - timeout handling
- live command dispatch
- live tool dispatch
- live lifecycle hook dispatch

Remaining work:

- Integrate the host into `pi-rust-core` and `pi-rust-cli`.
- Define settings/config for plugin discovery and enablement.
- Add provider and model execution dispatch.
- Add supervision, cancellation, and richer protocol evolution beyond the initial registration exchange.
- Add higher-level lifecycle coverage beyond the current command/tool/hook path.

## Project 5: Standalone Product Integration

Status: not started

Recommended first step:

- Extract the existing CLI-owned stdio RPC loop into a reusable Rust host/client layer.

Why this is next:

- The shared wire vocabulary already lives in `crates/pi-rust-protocol`.
- The runtime is already transport-neutral in `crates/pi-rust-core`.
- The missing piece is a reusable Rust-owned host/client seam rather than more protocol invention.

Scope for the first standalone integration slice:

- stdio-first reusable RPC host
- thin typed Rust client
- no daemon lifecycle yet
- no socket or websocket transport yet

## Explicit Non-Goals

These are not productization bugs and should not be framed as unfinished parity:

- direct JavaScript or TypeScript extension execution
- Node/Bun compatibility for extensions
- TypeScript-only runtime UI injection and event-bus behavior

Those are either inherent mismatches or future Rust-native product work, not remaining parity debt.
