# Contributing to cell

Thanks for helping improve the Rust `cell` repo.

## Repo boundaries

Do normal product work inside this repo.

- Use the Rust repo as the source of truth for code, history, tags, and releases.
- Use the outer TypeScript repo only when you intentionally need optional parity tooling.
- Treat `.pi/` state and `dist/` artifacts as local-only data that should not be committed.

## Local setup

From the repo root:

```bash
cargo run -p cell-cli -- --help
cargo run -p cell-cli --
```

If you are working on plugins, start with the plugin docs under [`docs/plugins/README.md`](./docs/plugins/README.md) and the runnable example under [`examples/plugin-hello/README.md`](./examples/plugin-hello/README.md).

## Verification

Run the normal checks before you open a pull request:

```bash
cargo test --workspace
cargo check --workspace --lib --bins
```

Run the Rust-only terminal regression suite when you change interactive behavior:

```bash
env -u PI_TS_REPO cargo test -p cell-cli --test tui_parity
```

Run the repo's standard verification flow when you touch CI, packaging, release flow, or wide cross-cutting behavior:

```bash
./scripts/rust_only_ci.sh
```

## Pull requests

- Keep each pull request focused on one change or closely related set of changes.
- Explain the user-visible effect in plain language.
- Update docs when commands, workflows, or support boundaries change.
- Add or update tests when behavior changes.
- Do not commit generated archives, checksums, or local `.pi` state.

## Releases

Maintainers should follow the release steps in [`docs/maintainers/release.md`](./docs/maintainers/release.md) and the packaging notes in [`docs/maintainers/packaging.md`](./docs/maintainers/packaging.md).
