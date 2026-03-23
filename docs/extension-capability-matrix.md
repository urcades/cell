# Extension Capability Matrix

This document tracks the remaining product-parity work around extensions and resource loading.

## Fixable now

- Skills, prompts, themes, and context resources loaded from global, project, and package roots
- Package install, remove, update, and list flows
- Startup and reload surfacing for static resources
- RPC control-plane parity around abort handling
- Pure-Rust plugin v1 contract for manifest, command/tool/flag registration, lifecycle hooks, and provider/model registration

## Still different but fixable

- Plugin loading and dispatch from the pure-Rust contract are still deferred
- Per-package manifest and resource filtering semantics
- A true `Resource Configuration` surface with per-item enable and disable behavior
- Broader end-to-end proof for provider auth, share/export, and package resource configuration

## Inherent mismatch / stop here

- Executing JS/TS extensions directly with Node runtime semantics
- Reproducing the full TypeScript extension runtime as-is, including JS handlers, event bus behavior, and injected custom UI surfaces

`pi-rust` stays pure Rust. Extension parity means matching capability classes in Rust where that is practical and user-visible, not embedding the TypeScript runtime.
