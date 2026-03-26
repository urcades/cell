# pi-rust Workspace

`pi-rust` is the Rust-only workspace for the pi product. It is kept nested beside the TypeScript monorepo, but the Rust repo is now intended to operate as its own source of truth.

Current coverage:
- Workspace and crate layout matching the product seams
- CLI, config, models, OAuth, packages, resources, session, tools, and TUI crates
- Pure-Rust plugin v1 contract crate with manifest and registration types
- Apple Terminal parity fixtures and Rust-native test coverage
- Session JSONL parsing and migration helpers
- Model resolution and provider registry helpers

Standalone workflow:
- `cargo test --workspace`
- `env -u PI_TS_REPO cargo test -p pi-rust-cli --test tui_parity`
- `scripts/rust_only_ci.sh`
- `scripts/package_rust_repo.sh`

Bridge tooling:
- Some parity-capture scripts still compare against the TypeScript checkout. Treat those as optional local integration tooling, not as required Rust-only development steps.
- The Rust-only CI and packaging scripts do not depend on `PI_TS_REPO`.
- Any TypeScript-dependent parity script must still be run with `PI_TS_REPO=/path/to/typescript/pi`.
- Without `PI_TS_REPO`, the Rust workspace tests stay Rust-only and skip TS bridge captures cleanly.

Current product focus:
- Rust-native plugin runtime for commands, tools, hooks, and diagnostics
- Rust-native plugin root management through `plugins`
- Standalone Rust packaging and CI without TypeScript repo coupling

Plugin parity:
- Static resources and package-fed resources are already largely present in Rust.
- Pure-Rust plugin v1 is now specified as a manifest plus registration contract in `crates/pi-rust-plugins`.
- Exact JS/TS extension execution is intentionally not the parity target for `pi-rust`.
- Remaining plugin work should be judged against Rust-native capability classes, not Node runtime embedding.
- See `docs/extension-capability-matrix.md` for the current fixable-vs-inherent split.

Plugin roots:
- Use `pi-rust plugins add-root <path>` to add a user-scoped root or `pi-rust plugins add-root <path> --project` to store it in `.pi/settings.json`.
- Use `pi-rust plugins add-root <path> --local` as a compatibility alias for the same project-scoped setting.
- Use `pi-rust plugins remove-root <path>` and `--project` or `--local` the same way to remove roots.
- Use `pi-rust plugins list --mode json` when you want the structured plugin runtime diagnostics payload, or omit `--mode json` for the text view.
- Discovery order is project package roots, user package roots, project `pluginRoots`, then user `pluginRoots`.

Packaging:
- `scripts/package_rust_repo.sh` builds `pi-rust` in release mode and writes a retained versioned archive for the current platform under `dist/releases/<version>/` by default.
- Use `--output <path>` if you want a different archive location.
- The packaging script also writes a checksum beside the archive.

Release flow:
- Run `scripts/rust_only_ci.sh` first.
- Build the archive with `scripts/package_rust_repo.sh`.
- Verify the checksum beside the archive.
- Create the nested Rust repo release commit.
- Tag that commit as `v<version>`.

Maintainer docs:
- `docs/port-postmortem.md` for the high-level port assessment, architecture map, brittle areas, and stop-rule recommendation.
- `docs/plugins/README.md` for plugin authoring, protocol, and capability-class guidance.
- `examples/plugin-hello/README.md` for the standalone runnable Rust plugin example.
