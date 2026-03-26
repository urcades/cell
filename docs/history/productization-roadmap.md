# Productization Roadmap (Archived)

This file records the productization program that turned the Rust port into a standalone product repo.

## Final state

All planned productization projects are complete.

- Project 1: Baseline Closure — complete
- Project 2: Product Hardening — complete
- Project 3: Resource and Model Platform — complete
- Project 4: Headless Plugin Host v1 — complete
- Project 5: Standalone Product Integration — complete

## What each project delivered

### Project 1: Baseline Closure

Delivered:

- green Rust-only terminal regression coverage
- a frozen Apple Terminal baseline
- parity residuals moved into either cleanup work or separate future tracks

### Project 2: Product Hardening

Delivered:

- safer config and settings writes
- interactive layer decomposition
- removal of parity-only scaffolding from active product paths

### Project 3: Resource and Model Platform

Delivered:

- Rust-owned model catalog flow
- separate available-versus-known model views
- stable resource and package precedence behavior
- supported resource toggle and writeback behavior

### Project 4: Headless Plugin Host v1

Delivered:

- out-of-process plugin discovery and launch
- versioned handshake and registration
- live command dispatch
- live tool dispatch
- live hook dispatch
- startup and runtime diagnostics

### Project 5: Standalone Product Integration

Delivered:

- shared JSON-line transport
- reusable RPC layer
- Rust-only verification and packaging flow
- standalone nested-repo release process
- plugin root management through the product itself

## Deferred future work

These items were intentionally left out of the active roadmap:

- plugin-provided provider execution
- plugin-provided model execution
- JavaScript and TypeScript extension execution
- Node/Bun compatibility
- injected custom plugin UI

## Why this file is archived

The productization roadmap is no longer an active plan. It is a record of the completed transition from parity-era Rust port to standalone Rust product.
