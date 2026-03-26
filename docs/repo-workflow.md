## Repo workflow

`/Users/edouard/Developer/pi/rust` is the source of truth for the Rust port.

- The outer TypeScript repo ignores `rust/`.
- Normal Rust work happens entirely inside the nested Rust repo.
- Rust development and CI must work without the TypeScript checkout.
- TypeScript parity capture is optional bridge tooling only.

## Rust-native workflow

Run commands from `/Users/edouard/Developer/pi/rust`.

- `cargo test --workspace`
- `env -u PI_TS_REPO cargo test -p pi-rust-cli --test tui_parity`
- `scripts/rust_only_ci.sh`
- `scripts/package_rust_repo.sh`

`scripts/rust_only_ci.sh` is the Rust-native verification entrypoint. It must pass even when `PI_TS_REPO` is unset.

Release procedure for the nested Rust repo:

1. Run the Rust-native verification entrypoint.
2. Build the retained archive with `scripts/package_rust_repo.sh`.
3. Verify the generated `.sha256` sidecar beside the archive.
4. Create the release-formalization commit in the nested Rust repo.
5. Tag that commit with `v<version>` in the nested Rust repo.

## Plugin-root workflow

Plugin discovery is configured from settings rather than plugin execution.

- `pi-rust plugins list --mode json` prints the structured plugin runtime diagnostics payload.
- `pi-rust plugins add-root <path>` stores a user-scoped root.
- `pi-rust plugins add-root <path> --project` stores the root in `.pi/settings.json`.
- `pi-rust plugins add-root <path> --local` remains a compatibility alias for the same project-scoped settings.
- `pi-rust plugins remove-root <path>` and `--project` or `--local` remove roots from the same scopes.
- Discovery order is project package roots, user package roots, project `pluginRoots`, then user `pluginRoots`.

## Plugin-author workflow

Use the standalone example when you want to exercise the Rust plugin contract end to end without adding anything to the main workspace build.

- `cargo test --manifest-path examples/plugin-hello/Cargo.toml` runs the example crate's integration test.
- `cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- launch examples/plugin-hello/pi-plugin-host.json` launches the example through the host and prints the registered capability summary.
- The example crate has its own `Cargo.toml` workspace boundary so the main workspace does not try to build it automatically.

## Parity bridge workflow

TypeScript side-by-side capture is opt-in.

- Set `PI_TS_REPO` to the TypeScript checkout root when you want parity comparisons.
- If `PI_TS_REPO` is unset, Rust-native development still works and parity bridge commands should fail clearly rather than guessing parent-relative paths.
- The Rust-only CI and packaging scripts do not depend on `PI_TS_REPO`.

## Packaging workflow

- `scripts/package_rust_repo.sh` builds the release `pi-rust` binary for the current platform.
- By default it writes a retained versioned archive to `dist/releases/<version>/pi-rust-<version>-<target>.tar.gz`.
- Use `--output <path>` when CI or local automation needs a different archive path.
- The archive contains the release binary plus the maintainer-facing workflow docs needed to operate the nested Rust repo.
- The checksum is written beside the archive as `<archive>.sha256`.

## Git workflow

- Use the outer repo only for TypeScript changes and for the single ignore rule that excludes `rust/`.
- Use the nested Rust repo for Rust changes, history, branches, and releases.
- Do not rely on the outer repo to track Rust work.
