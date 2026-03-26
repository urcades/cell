# Runnable Example Plugin

The example plugin lives at [examples/plugin-hello](../../examples/plugin-hello).

It is intentionally small:

- one descriptor
- one Rust entrypoint
- one handshake
- one registration manifest
- one command
- one tool
- one hook

## What it demonstrates

- A descriptor can point at `cargo run` instead of a prebuilt binary.
- The plugin can answer the host handshake over stdio.
- The plugin can register live command, tool, and hook capabilities.
- The plugin can be launched outside the main workspace build.

## How to run it

From the repo root:

```bash
cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- launch examples/plugin-hello/pi-plugin-host.json
```

To run the example crate's own integration test:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml
```

## What to expect

The host should report one registered plugin with one command, one tool, and one hook.
