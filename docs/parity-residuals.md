# Final parity residuals

This file is the stop-rule checkpoint for the Rust replica.

## Fixed in the current pass

- Nested Rust repo now has its own history and Rust-only CI
- TypeScript parity tooling is documented as optional bridge tooling
- Package config accepts string and object entries with per-resource arrays
- Package discovery understands `package.json` `pi` resource manifests
- Package/resource discovery respects package filters and ignore files conservatively
- Project package/resource precedence beats user scope when identities collide
- The standalone `Resource Configuration` surface is resource-first instead of package-only
- Live working status moved out of the transcript body
- The old settled `Response received.` line is gone
- Interactive export wording and CLI export wording now match the TypeScript phrasing more closely
- The export HTML is now a richer standalone viewer owned by Rust
- A pure-Rust plugin v1 contract exists as a dedicated crate and spec

## Still different but fixable

- `Resource Configuration` still shows enabled resources more clearly than disabled ones; full per-item toggle/writeback parity is not finished
- Active-run microcopy is closer, but not every low-traffic status string is guaranteed identical
- Footer semantics are closer, but provider/subscription cases are still not fully proven across every model/provider combination
- `/share` is improved in wording, but it still does not replicate every TypeScript in-progress and cancel-state nuance

## Inherent mismatch / stop here

- Direct JS/TS extension execution with Node/Bun runtime semantics
- Full TypeScript event-bus and injected custom UI runtime behavior
- TypeScript-only extension surfaces that depend on embedding the JS runtime rather than matching the same capability class in Rust
