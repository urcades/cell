# pi-rust Workspace

`pi-rust` is the Rust-only workspace for the pi port. It is kept nested beside the TypeScript monorepo while the parity work is still active, but it is intended to stand on its own as a separate Rust project.

Current coverage:
- Workspace and crate layout matching the product seams
- CLI, config, models, OAuth, packages, resources, session, tools, and TUI crates
- Pure-Rust plugin v1 contract crate with manifest and registration types
- Apple Terminal parity fixtures and Rust-native test coverage
- Session JSONL parsing and migration helpers
- Model resolution and provider registry helpers

Standalone workflow:
- `cargo test --workspace`
- `cargo test -p pi-rust-cli --test tui_parity`

Bridge tooling:
- Some parity-capture scripts still compare against the TypeScript checkout. Treat those as optional local integration tooling, not as required Rust-only development steps.
- Any TypeScript-dependent parity script must be run with `PI_TS_REPO=/path/to/typescript/pi`.
- Without `PI_TS_REPO`, the Rust workspace tests stay Rust-only and skip TS bridge captures cleanly.

Known remaining work:
- Final Apple Terminal polish on the TUI
- Higher-level parity gaps outside the TUI, including extension capability coverage and a few remaining control-plane edges

Extension parity:
- Static resources and package-fed resources are already largely present in Rust.
- Pure-Rust plugin v1 is now specified as a manifest plus registration contract in `crates/pi-rust-plugins`.
- Exact JS/TS extension execution is intentionally not the parity target for `pi-rust`.
- Remaining extension work should be judged against Rust-native capability classes, not Node runtime embedding.
- See `docs/extension-capability-matrix.md` for the current fixable-vs-inherent split.
