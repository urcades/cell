# Plugin Capability Matrix

This document tracks the live-versus-deferred split for the Rust plugin host.

## Live capability classes

- Descriptor discovery from `pi-plugin-host.json` and `plugin-host.json`
- Handshake and manifest registration over stdio
- Command dispatch
- Tool dispatch
- Lifecycle hook dispatch
- Startup diagnostics and duplicate capability rejection

## Deferred capability classes

- Plugin flags as a live CLI/runtime surface
- Provider execution
- Model execution
- Per-package manifest and resource filtering semantics for plugin runtime behaviors
- Custom UI surfaces

## Inherent mismatch

- Embedding the TypeScript extension runtime or executing JS/TS extensions directly

## Notes

- Provider and model registrations are already accepted and merged, but they are not dispatched yet.
- Flags are already part of the manifest and startup summary, but they are not wired into a live plugin flag surface yet.
- `pi-rust` stays pure Rust; parity means matching the practical capability class, not reproducing the TypeScript runtime literally.
