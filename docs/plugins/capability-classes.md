# Capability Classes

This is the short-form reference for plugin capability status.

## Live now

| Class | Status | Notes |
| --- | --- | --- |
| Descriptor discovery | Live | The host finds `cell-plugin-host.json` and `plugin-host.json` under configured roots. |
| Handshake and registration | Live | The host expects a JSON-line handshake and a versioned manifest. |
| Commands | Live | Commands are dispatched with `command_request` and `command_response`. |
| Tools | Live | Tools are dispatched with `tool_request` and `tool_response`. |
| Lifecycle hooks | Live | Hooks are dispatched with `hook_request` and `hook_response`. |

## Accepted, but not live

| Class | Status | Notes |
| --- | --- | --- |
| Flags | Registered only | Visible in manifests and diagnostics, not yet a live runtime flag surface. |
| Providers | Deferred | Registrations are accepted and summarized, but provider execution is not live. |
| Models | Deferred | Registrations are accepted and summarized, but model execution is not live. |

## Not a target

| Class | Status | Notes |
| --- | --- | --- |
| JS/TS extension runtime embedding | Out of scope | `cell` stays pure Rust. Matching the old JS runtime literally is not a goal. |

For the fuller author-facing explanation, see [Capabilities](./capabilities.md) and the product-level [Plugin Capability Matrix](../extension-capability-matrix.md).
