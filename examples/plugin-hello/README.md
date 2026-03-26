# Hello Plugin Example

This is a standalone Rust plugin example that stays outside the main workspace build.

## Layout

- `Cargo.toml` defines a tiny isolated workspace.
- `pi-plugin-host.json` describes how the host launches the plugin.
- `src/main.rs` is the executable entrypoint.
- `tests/runtime.rs` verifies the live command, tool, and hook paths.

## Run it

From the repo root:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml
```

Or launch it through the host:

```bash
cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- launch examples/plugin-hello/pi-plugin-host.json
```

## What it does

- Command: `hello`
- Tool: `echo`
- Hook: `session-started`
