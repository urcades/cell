# cell

`cell` is the Rust codebase for the Cell CLI and terminal UI. This repo is intended to stand on its own: you can build it, run it, test it, package it, and develop Rust-native plugins from here without needing the TypeScript tree for normal work.

## Quick start

Run everything from the Rust repo root:

```bash
cd /Users/edouard/Developer/pi/rust
```

Show the main help:

```bash
cargo run -p cell-cli -- --help
```

Start the interactive app:

```bash
cargo run -p cell-cli --
```

Run one prompt and exit:

```bash
cargo run -p cell-cli -- -p "Summarize the files in this directory"
```

Run the normal verification flow:

```bash
cargo test --workspace
env -u PI_TS_REPO cargo test -p cell-cli --test tui_parity
./scripts/rust_only_ci.sh
```

Build the current-platform release archive:

```bash
./scripts/package_rust_repo.sh
```

## What this repo contains

- `cell`, the main CLI and TUI application
- The supporting crates for sessions, models, config, resources, packages, tools, OAuth, transport, and RPC
- The Rust-native plugin host, protocol, and manifest crates
- A runnable example plugin under `examples/plugin-hello`
- Rust-only verification and packaging scripts

## What you do not need for normal work

You do not need the TypeScript repo for day-to-day Rust development.

The only remaining TypeScript linkage is optional parity tooling. If you are not doing side-by-side capture work, leave `PI_TS_REPO` unset.

## Prerequisites

- A working Rust toolchain
- Standard terminal tooling available on your machine
- API credentials for whichever model provider you want to use

Common environment variables:

- `ANTHROPIC_API_KEY`
- `ANTHROPIC_OAUTH_TOKEN`
- `OPENAI_API_KEY`
- `OPENROUTER_API_KEY`
- `CELL_CODING_AGENT_DIR` to override session storage

Compatibility variables still supported today:

- `PI_PACKAGE_DIR`
- `PI_SHARE_VIEWER_URL`

## Running from source

Main help:

```bash
cargo run -p cell-cli -- --help
```

Interactive mode:

```bash
cargo run -p cell-cli --
```

Non-interactive mode:

```bash
cargo run -p cell-cli -- -p "Review the Rust crates in this repo"
```

List currently usable models:

```bash
cargo run -p cell-cli -- --list-models
```

List the full known catalog with diagnostic auth status:

```bash
cargo run -p cell-cli -- --list-known-models
```

## Plugin system

`cell` supports Rust-native executable plugins.

Live capability classes today:

- commands
- tools
- hooks

Deferred capability classes:

- flags as a live runtime surface
- provider execution
- model execution

Not supported:

- JavaScript or TypeScript extension execution
- embedding Node or Bun into the Rust runtime
- custom injected plugin UI

Useful plugin commands from source:

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
cargo run -p cell-cli -- plugins add-root /absolute/path/to/plugins
cargo run -p cell-cli -- plugins remove-root /absolute/path/to/plugins
```

Project-scoped plugin roots are currently stored in `.pi/settings.json`. That path is part of the compatibility surface even though the product name is now `cell`.

If you want to inspect plugin discovery and launch behavior directly, use the host:

```bash
cargo run -p cell-plugin-host -- --help
cargo run -p cell-plugin-host -- discover examples/plugin-hello
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```

## Verification and release

Normal verification:

```bash
cargo test --workspace
```

Rust-only terminal regression suite:

```bash
env -u PI_TS_REPO cargo test -p cell-cli --test tui_parity
```

Rust-native verification entrypoint:

```bash
./scripts/rust_only_ci.sh
```

Release packaging:

```bash
./scripts/package_rust_repo.sh
```

By default, the packaging script writes:

```text
dist/releases/<version>/cell-<version>-<target>.tar.gz
dist/releases/<version>/cell-<version>-<target>.tar.gz.sha256
```

## Documentation map

- [`docs/README.md`](./docs/README.md): documentation index
- [`docs/architecture.md`](./docs/architecture.md): current architecture overview
- [`docs/repo-workflow.md`](./docs/repo-workflow.md): maintainer landing page
- [`docs/maintainers/README.md`](./docs/maintainers/README.md): maintainer docs index
- [`docs/plugins/README.md`](./docs/plugins/README.md): plugin author guide
- [`examples/plugin-hello/README.md`](./examples/plugin-hello/README.md): runnable example plugin walkthrough
- [`docs/history/README.md`](./docs/history/README.md): archived roadmap, parity, and postmortem material

## Compatibility notes

A few older compatibility surfaces still exist:

- project-scoped settings still live under `.pi/settings.json`
- some environment variables still use `PI_*` names
- optional parity tooling still refers to `PI_TS_REPO`

Those are deliberate compatibility holdovers, not signs that the Rust repo depends on the outer project.
