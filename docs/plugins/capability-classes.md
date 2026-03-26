# Capability Classes

This document separates what is live in the current Rust host from what is still deferred.

## Live now

| Class | Status | Notes |
| --- | --- | --- |
| Descriptor discovery | Live | The host finds `pi-plugin-host.json` and `plugin-host.json` files under the configured roots. |
| Handshake and registration | Live | The host expects a JSON-line handshake and a versioned manifest registration. |
| Commands | Live | Commands are dispatched back to the plugin with `CommandRequest` and `CommandResponse`. |
| Tools | Live | Tools are dispatched back to the plugin with `ToolRequest` and `ToolResponse`. |
| Lifecycle hooks | Live | Hooks are dispatched back to the plugin with `HookRequest` and `HookResponse`. |

## Registered, but deferred

| Class | Status | Notes |
| --- | --- | --- |
| Flags | Registered only | The manifest and summaries carry flag metadata, but there is no live plugin flag surface yet. |
| Providers | Catalog only | Provider registrations are merged and deduplicated, but provider execution is still deferred. |
| Models | Catalog only | Model registrations are merged and deduplicated, but model execution is still deferred. |

## Inherent mismatch

| Class | Status | Notes |
| --- | --- | --- |
| JS/TS extension runtime embedding | Not a target | `pi-rust` stays pure Rust. Matching the TypeScript runtime by embedding JS is explicitly out of scope. |

## Practical reading

If a capability class is in the live table, the current host can use it in real runtime flows.
If it is in the deferred table, the host only accepts and records it for now.
